use {
    crate::{
        accounts::AccountsDB,
        rpc::{
            server::{start_rpc_service, RpcServiceConfig},
            ReadDeps, WriteDeps,
        },
        scheduler::ConflictFreeBatch,
        stages::{
            dedup::load_dedup_state, execution::start_execution_worker,
            sequencer::start_sequence_worker, settle::start_settle_worker,
            sigverify::start_sigverify_workerpool, AccountSettlement,
        },
    },
    futures::future::FutureExt,
    solana_hash::Hash,
    solana_sdk::{pubkey::Pubkey, transaction::SanitizedTransaction},
    solana_svm::transaction_processor::LoadAndExecuteSanitizedTransactionsOutput,
    std::time::Duration,
    tokio::{sync::mpsc, task::JoinHandle},
    tokio_mpmc,
    tokio_util::sync::CancellationToken,
    tracing::{error, info, warn},
};

#[derive(Debug, Clone, PartialEq, clap::ValueEnum)]
pub enum NodeMode {
    /// Read-only node - serves read RPCs only
    Read,
    /// Write-only node - processes transactions only
    Write,
    /// All-in-one - both read and write
    Aio,
}

#[derive(Clone)]
pub struct NodeConfig {
    pub mode: NodeMode,
    pub port: u16,
    pub sigverify_queue_size: usize,
    pub sigverify_workers: usize,
    pub max_connections: usize,
    pub max_tx_per_batch: usize,
    pub accountsdb_connection_url: String,
    pub admin_keys: Vec<Pubkey>, // Admin keys that can bypass SPL token program execution
    pub transaction_expiration_ms: u64,
    pub blocktime_ms: u64,
    pub perf_sample_period_secs: u64, // Performance sample collection period (default 60 seconds)
}

impl NodeConfig {
    /// Calculate max_blockhashes from transaction_expiration_ms and blocktime_ms
    /// This represents how many blockhashes we need to keep in the dedup cache
    pub fn max_blockhashes(&self) -> usize {
        (self.transaction_expiration_ms / self.blocktime_ms) as usize
    }
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            mode: NodeMode::Aio, // Default to all-in-one mode
            port: 8899,
            sigverify_queue_size: 1000,
            sigverify_workers: 4,
            max_connections: 100,
            max_tx_per_batch: 64,
            accountsdb_connection_url: "postgresql://user:password@localhost:5432/contra"
                .to_string(),
            admin_keys: vec![],               // No admin keys by default
            transaction_expiration_ms: 15000, // 15 seconds default
            blocktime_ms: 100,                // 100ms default
            perf_sample_period_secs: 60,      // 60 seconds default
        }
    }
}

pub struct WorkerHandle {
    name: String,
    pub(crate) handle: JoinHandle<()>,
}

impl WorkerHandle {
    pub fn new(name: String, handle: JoinHandle<()>) -> Self {
        Self { name, handle }
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

pub struct NodeHandles {
    workers: Vec<WorkerHandle>,
    shutdown_token: CancellationToken,
}

pub async fn run_node(config: NodeConfig) -> Result<NodeHandles, Box<dyn std::error::Error>> {
    // Validate configuration
    if config.blocktime_ms == 0 && matches!(config.mode, NodeMode::Write | NodeMode::Aio) {
        return Err("blocktime_ms cannot be 0 for write nodes".into());
    }
    if config.max_blockhashes() == 0 && matches!(config.mode, NodeMode::Write | NodeMode::Aio) {
        return Err(
            "transaction_expiration_ms must be >= blocktime_ms (max_blockhashes would be 0)".into(),
        );
    }

    // Create a single shutdown token for all services
    let shutdown_token = CancellationToken::new();

    // Only create write pipeline for Write and Aio modes
    let mut write_workers: Vec<WorkerHandle> = Vec::new();
    let (write_deps, live_blockhashes_arc) =
        if matches!(config.mode, NodeMode::Write | NodeMode::Aio) {
            // Create the dedup channel (receives from RPC, sends to sigverify) - unbounded
            let (dedup_tx, dedup_rx) = crate::stages::create_dedup_channel();

            // Create the sigverify channel (needed for NodeHandles in all modes)
            let (sigverify_tx, sigverify_rx) =
                tokio_mpmc::channel::<SanitizedTransaction>(config.sigverify_queue_size);

            // Create sequencer channel (unbounded mpsc for single consumer)
            let (sequencer_tx, sequencer_rx) = mpsc::unbounded_channel::<SanitizedTransaction>();

            // Create batch channel between sequencer and executor (unbounded for pipelining)
            let (batch_tx, batch_rx) = mpsc::unbounded_channel::<ConflictFreeBatch>();

            // Create execution results channel between executor and settler (unbounded for pipelining)
            let (execution_results_tx, execution_results_rx) = mpsc::unbounded_channel::<(
                LoadAndExecuteSanitizedTransactionsOutput,
                Vec<SanitizedTransaction>,
            )>();

            // Create settled accounts channel between settler and executor
            let (settled_accounts_tx, settled_accounts_rx) =
                mpsc::unbounded_channel::<Vec<(Pubkey, AccountSettlement)>>();

            // Create settled blockhashes channel between settler and dedup
            let (settled_blockhashes_tx, settled_blockhashes_rx) =
                mpsc::unbounded_channel::<Hash>();

            // Load persisted dedup state from DB before starting the stage.
            // Failure here is fatal: starting with an empty cache could allow
            // duplicate transactions to execute after a restart.
            let db = AccountsDB::new(&config.accountsdb_connection_url, true).await?;
            let (initial_live_blockhashes, initial_dedup_cache) =
                load_dedup_state(&db, config.max_blockhashes()).await?;

            // Start dedup stage (filters duplicate transactions before sigverify)
            let (dedup, live_blockhashes) = crate::stages::start_dedup(crate::stages::DedupArgs {
                max_blockhashes: config.max_blockhashes(),
                input_rx: dedup_rx,
                settled_blockhashes_rx,
                output_tx: sigverify_tx.clone(),
                shutdown_token: shutdown_token.clone(),
                initial_live_blockhashes,
                initial_dedup_cache,
            })
            .await;
            write_workers.push(dedup);

            // Start sigverify worker pool
            let sigverify_workers = start_sigverify_workerpool(crate::stages::SigverifyArgs {
                num_workers: config.sigverify_workers,
                admin_keys: config.admin_keys.clone(),
                rx: sigverify_rx,
                sequencer_tx,
                shutdown_token: shutdown_token.clone(),
            })
            .await;
            write_workers.extend(sigverify_workers);

            // Start sequencer (produces conflict-free batches)
            let sequence = start_sequence_worker(crate::stages::SequencerArgs {
                max_tx_per_batch: config.max_tx_per_batch,
                rx: sequencer_rx,
                batch_tx,
                shutdown_token: shutdown_token.clone(),
            })
            .await;
            write_workers.push(sequence);

            // Start executor (executes and settles batches)
            let execution = start_execution_worker(crate::stages::ExecutionArgs {
                batch_rx,
                settled_accounts_rx,
                execution_results_tx,
                accountsdb_connection_url: config.accountsdb_connection_url.clone(),
                shutdown_token: shutdown_token.clone(),
            })
            .await;
            write_workers.push(execution);

            let settle = start_settle_worker(crate::stages::SettleArgs {
                execution_results_rx,
                settled_accounts_tx,
                settled_blockhashes_tx,
                accountsdb_connection_url: config.accountsdb_connection_url.clone(),
                blocktime_ms: config.blocktime_ms,
                perf_sample_period_secs: config.perf_sample_period_secs,
                shutdown_token: shutdown_token.clone(),
            })
            .await;
            write_workers.push(settle);

            (
                Some(WriteDeps {
                    dedup_tx: dedup_tx.clone(),
                }),
                live_blockhashes,
            )
        } else {
            // Read-only node: no write pipeline, create empty live_blockhashes Arc
            use std::collections::LinkedList;
            use std::sync::{Arc, RwLock};
            (None, Arc::new(RwLock::new(LinkedList::new())))
        };

    // Start RPC service based on node mode
    let rpc_config = RpcServiceConfig {
        port: config.port,
        max_connections: config.max_connections,
        read_deps: match config.mode {
            NodeMode::Read | NodeMode::Aio => Some(ReadDeps {
                admin_keys: config.admin_keys,
                accounts_db: AccountsDB::new(&config.accountsdb_connection_url, true)
                    .await
                    .unwrap(),
                live_blockhashes: live_blockhashes_arc,
            }),
            NodeMode::Write => None,
        },
        write_deps,
        shutdown_token: shutdown_token.clone(),
    };
    let rpc_handle = start_rpc_service(rpc_config).await?;

    info!("Contra node started:");
    info!("  Mode: {:?}", config.mode);
    info!("  RPC port: {}", config.port);
    if matches!(config.mode, NodeMode::Write | NodeMode::Aio) {
        info!("  Sigverify workers: {}", config.sigverify_workers);
        info!("  Max transactions per batch: {}", config.max_tx_per_batch);
    }
    info!("  Max connections: {}", config.max_connections);

    // Build vector of all worker handles
    let mut workers = vec![rpc_handle];
    workers.extend(write_workers);

    Ok(NodeHandles {
        workers,
        shutdown_token,
    })
}

impl NodeHandles {
    /// Wait for any worker to quit
    /// Returns the name of the worker that quit
    pub async fn wait_for_any_worker_quit(&mut self) -> String {
        // Use futures::future::select_all to wait for any handle to complete
        let futures: Vec<_> = self
            .workers
            .iter_mut()
            .enumerate()
            .map(|(idx, worker)| {
                let future = (&mut worker.handle).map(move |_| idx);
                Box::pin(future)
            })
            .collect();

        let (completed_idx, _result, _remaining) = futures::future::select_all(futures).await;
        let worker_name = self.workers[completed_idx].name().to_string();

        error!("{} worker quit unexpectedly", worker_name);
        worker_name
    }

    pub async fn shutdown(self) {
        info!("Shutting down node...");

        // Cancel the token - this signals all services to shutdown
        self.shutdown_token.cancel();

        // Wait for all workers to finish
        for worker in self.workers {
            match tokio::time::timeout(Duration::from_secs(5), worker.handle).await {
                Ok(Ok(_)) => info!("{} stopped gracefully", worker.name),
                Ok(Err(e)) => error!("{} error: {:?}", worker.name, e),
                Err(_) => warn!("{} shutdown timeout", worker.name),
            }
        }

        info!("Node shutdown complete");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_sdk::signature::{Keypair, Signer};

    #[test]
    fn test_sequencer_config_with_admin_keys() {
        let admin1 = Keypair::new().pubkey();
        let admin2 = Keypair::new().pubkey();

        let config = NodeConfig {
            admin_keys: vec![admin1, admin2],
            ..Default::default()
        };

        assert_eq!(config.admin_keys.len(), 2);
        assert!(config.admin_keys.contains(&admin1));
        assert!(config.admin_keys.contains(&admin2));
    }

    #[test]
    fn test_node_config_default_has_no_admin_keys() {
        let config = NodeConfig::default();
        assert!(config.admin_keys.is_empty());
    }

    #[tokio::test]
    async fn test_run_node_rejects_zero_blocktime() {
        let config = NodeConfig {
            blocktime_ms: 0,
            ..Default::default()
        };

        let result = run_node(config).await;
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert_eq!(err.to_string(), "blocktime_ms cannot be 0 for write nodes");
    }

    #[tokio::test]
    async fn test_run_node_rejects_zero_max_blockhashes() {
        // transaction_expiration_ms < blocktime_ms → max_blockhashes() == 0
        let config = NodeConfig {
            transaction_expiration_ms: 50,
            blocktime_ms: 100,
            ..Default::default()
        };

        assert_eq!(config.max_blockhashes(), 0);
        let result = run_node(config).await;
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert_eq!(
            err.to_string(),
            "transaction_expiration_ms must be >= blocktime_ms (max_blockhashes would be 0)"
        );
    }

    #[test]
    fn test_node_config_defaults() {
        let config = NodeConfig::default();
        assert_eq!(config.port, 8899);
        assert_eq!(config.sigverify_queue_size, 1000);
        assert_eq!(config.sigverify_workers, 4);
        assert_eq!(config.max_connections, 100);
        assert_eq!(config.max_tx_per_batch, 64);
        assert_eq!(config.transaction_expiration_ms, 15000);
        assert_eq!(config.blocktime_ms, 100);
        assert_eq!(config.perf_sample_period_secs, 60);
        assert!(matches!(config.mode, NodeMode::Aio));
    }

    #[test]
    fn test_max_blockhashes_calculation() {
        let config = NodeConfig {
            transaction_expiration_ms: 15000,
            blocktime_ms: 100,
            ..Default::default()
        };
        assert_eq!(config.max_blockhashes(), 150);

        let config2 = NodeConfig {
            transaction_expiration_ms: 1000,
            blocktime_ms: 500,
            ..Default::default()
        };
        assert_eq!(config2.max_blockhashes(), 2);
    }

    #[tokio::test]
    async fn test_worker_handle_name() {
        let handle = WorkerHandle::new("test-worker".to_string(), tokio::spawn(async {}));
        assert_eq!(handle.name(), "test-worker");
    }

    #[test]
    fn test_node_mode_variants() {
        // Ensure all variants are distinct
        assert_ne!(NodeMode::Read, NodeMode::Write);
        assert_ne!(NodeMode::Write, NodeMode::Aio);
        assert_ne!(NodeMode::Read, NodeMode::Aio);
    }

    #[test]
    fn test_node_mode_read_skips_write_validation_check() {
        // Verify the validation logic: Read mode does NOT match Write | Aio
        assert!(!matches!(NodeMode::Read, NodeMode::Write | NodeMode::Aio));
        assert!(matches!(NodeMode::Write, NodeMode::Write | NodeMode::Aio));
        assert!(matches!(NodeMode::Aio, NodeMode::Write | NodeMode::Aio));
    }
}
