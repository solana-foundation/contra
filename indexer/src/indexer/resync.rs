use crate::{
    config::{BackfillConfig, ProgramType},
    error::{indexer::ReconciliationError, DataSourceError, IndexerError, StorageError},
    indexer::{
        backfill::BackfillService, checkpoint::CheckpointWriter,
        datasource::rpc_polling::rpc::RpcPoller, transaction_processor::TransactionProcessor,
    },
    operator::{
        enumerate_consumed_mints, ConsumedSet, RetryConfig, RpcClientWithRetry,
        CONSUMED_SET_PAGE_SIZE,
    },
    storage::common::storage::live_lock::{LiveLockMode, LIVE_LOCK_HEARTBEAT_INTERVAL},
    storage::Storage,
};
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::pubkey::Pubkey;
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

/// How to reach the PrivateChannel and whose mints to enumerate for the consumed-set.
#[derive(Clone, Debug)]
pub struct ChannelReconcileConfig {
    /// PrivateChannel RPC URL (the chain where mints land).
    pub channel_rpc_url: String,
    /// Mint authority (admin) whose confirmed mints carry the idempotency memos.
    pub authority: Pubkey,
}

/// Metric label for the live-state lock this service holds.
const RESYNC_LOCK_ROLE: &str = "resync";

/// Resync service for rebuilding indexer database from chain history
pub struct ResyncService {
    storage: Arc<Storage>,
    rpc_poller: Arc<RpcPoller>,
    program_type: ProgramType,
    backfill_config_base: BackfillConfig,
    escrow_instance_id: Option<Pubkey>,
    // When set, the rebuild reconciles each row against the channel's existing mints and
    // fails closed if that set cannot be built. None preserves the legacy rebuild.
    channel_reconcile: Option<ChannelReconcileConfig>,
    // How often the live-state lock re-proves itself. Only tests override it.
    lock_heartbeat_interval: Duration,
}

impl ResyncService {
    pub fn new(
        storage: Arc<Storage>,
        rpc_poller: Arc<RpcPoller>,
        program_type: ProgramType,
        backfill_config_base: BackfillConfig,
        escrow_instance_id: Option<Pubkey>,
    ) -> Self {
        Self {
            storage,
            rpc_poller,
            program_type,
            backfill_config_base,
            escrow_instance_id,
            channel_reconcile: None,
            lock_heartbeat_interval: LIVE_LOCK_HEARTBEAT_INTERVAL,
        }
    }

    /// Probe the live-state lock on `interval` instead of the production one, so a
    /// test can drive a lock loss without waiting out a rebuild.
    pub fn with_lock_heartbeat_interval(mut self, interval: Duration) -> Self {
        self.lock_heartbeat_interval = interval;
        self
    }

    /// Enable reconcile-on-rebuild against the PrivateChannel's existing mints.
    pub fn with_channel_reconcile(mut self, config: ChannelReconcileConfig) -> Self {
        self.channel_reconcile = Some(config);
        self
    }

    /// Build the consumed-set from the channel, failing closed on any error.
    ///
    /// The production entrypoint (run_resync) guarantees a channel RPC is configured and
    /// refuses to run otherwise, so the None branch below (warn + Ok(None)) only applies to
    /// direct/test construction.
    async fn build_consumed_set(&self) -> Result<Option<Arc<ConsumedSet>>, IndexerError> {
        let Some(reconcile) = self.channel_reconcile.as_ref() else {
            warn!(
                "Resync running WITHOUT channel reconciliation (no channel RPC configured); \
                 rebuilt deposit/withdrawal rows will be pending. Only safe on an empty channel."
            );
            return Ok(None);
        };

        info!(
            "Building consumed-set from PrivateChannel authority {} before any destruction...",
            reconcile.authority
        );
        let channel_rpc = RpcClientWithRetry::with_retry_config(
            reconcile.channel_rpc_url.clone(),
            RetryConfig::default(),
            CommitmentConfig::confirmed(),
        );
        let set =
            enumerate_consumed_mints(&channel_rpc, &reconcile.authority, CONSUMED_SET_PAGE_SIZE)
                .await
                .map_err(|reason| {
                    error!(
                        authority = %reconcile.authority,
                        "Consumed-set enumeration failed; aborting resync before drop: {reason}"
                    );
                    IndexerError::Reconciliation(ReconciliationError::ConsumedSetUnavailable {
                        reason,
                    })
                })?;
        info!(
            "Consumed-set built: {} serviced mint(s) on the channel",
            set.len()
        );
        Ok(Some(Arc::new(set)))
    }

    /// Run the resync process
    /// Returns Ok(()) if resync successful, Err otherwise
    pub async fn run(&self, genesis_slot: u64) -> Result<(), IndexerError> {
        info!(
            "Starting database resync for {:?} from slot {}...",
            self.program_type, genesis_slot
        );

        // ---- Pre-flight: every check runs BEFORE any destruction (fail closed). ----
        // On any failure below we return Err with the live DB completely untouched, so a
        // future-slot, an unreachable channel, a legacy-scheme memo, a live worker or an
        // unresolved halt can never leave a half-wiped database.

        // Pre-flight 0: take the live-state lock exclusively, before anything else.
        // It is what proves no indexer or operator is writing to this database, and
        // holding it for the whole rebuild also refuses any worker that tries to start
        // while we run. Every later check is pointless without it.
        let lock_lost = CancellationToken::new();
        let live_lock = self
            .storage
            .try_acquire_live_lock(
                LiveLockMode::Exclusive,
                RESYNC_LOCK_ROLE,
                lock_lost.clone(),
                self.lock_heartbeat_interval,
            )
            .await
            .inspect_err(|e| error!("Refusing to resync: {}", e))?;
        info!("Live-state lock acquired; no indexer or operator can run against this database");

        // The halt read below needs the tables to exist, and a resync is also the
        // supported way to build a database from nothing. Creating the schema is
        // idempotent and matches what both workers do at startup.
        self.storage.init_schema().await?;

        // Pre-flight 0b: a reconciliation halt means custody and the ledger already
        // disagree. The rebuild drops the table the flag lives in, so running now would
        // clear an unresolved halt and destroy the evidence behind it.
        if let Some(halt) = self.storage.is_reconciliation_halted().await? {
            error!(
                "Refusing to resync a halted database; halt reason: {}",
                halt.reason
            );
            return Err(IndexerError::Reconciliation(
                ReconciliationError::ReconciliationHalted {
                    reason: halt.reason,
                },
            ));
        }

        // Pre-flight 1: an escrow rebuild needs its instance scope. The processor filters
        // escrow instructions by it and an unset scope drops every one of them, so a rebuild
        // without it would empty the tables, refill them with nothing, and still advance the
        // checkpoint to the tip. That leaves no gap for a later run to detect, which makes it
        // the one failure here that is not recoverable by repeating the operation.
        if self.program_type == ProgramType::Escrow && self.escrow_instance_id.is_none() {
            error!("Refusing to resync the escrow indexer with no escrow instance id");
            return Err(IndexerError::Reconciliation(
                ReconciliationError::InvalidPubkey {
                    pubkey: "<missing>".to_string(),
                    reason: "escrow_instance_id is required to resync the escrow indexer"
                        .to_string(),
                },
            ));
        }

        // Pre-flight 2: genesis_slot must not be ahead of the chain tip.
        let current_slot = self.rpc_poller.get_latest_slot().await.map_err(|e| {
            error!("Failed to fetch current slot before resync backfill: {}", e);
            IndexerError::DataSource(e.into())
        })?;
        if genesis_slot > current_slot {
            error!(
                "Invalid genesis_slot {}: cannot be ahead of current_slot {}",
                genesis_slot, current_slot
            );
            return Err(IndexerError::from(DataSourceError::InvalidConfig {
                reason: format!(
                    "genesis_slot {} is ahead of current_slot {}",
                    genesis_slot, current_slot
                ),
            }));
        }

        // Pre-flight 3+4: channel reachability + consumed-set completeness + cross-scheme
        // guard, all inside build_consumed_set, which returns Err on any of them.
        let consumed = self.build_consumed_set().await?;

        // ---- Destruction: only now, with a complete consumed-set in hand. ----
        // The heartbeat bounds a silently lost lock to one interval, which is fine for a
        // worker that only has to stop. The drop is irreversible, so prove ownership
        // synchronously here instead of trusting the last tick.
        live_lock.ensure_held().await.inspect_err(|e| {
            error!("Refusing to drop tables: {}", e);
        })?;

        // Step 1: Drop existing tables
        info!("Dropping existing database tables...");
        self.storage.drop_tables().await.map_err(|e| {
            error!("Failed to drop database tables during resync: {}", e);
            e
        })?;
        info!("Database tables dropped successfully");

        // Step 2: Recreate schema
        info!("Recreating database schema...");
        self.storage.init_schema().await.map_err(|e| {
            error!("Failed to recreate database schema during resync: {}", e);
            e
        })?;
        info!("Database schema recreated successfully");

        // Step 3: Create BackfillService with genesis_slot configuration
        let backfill_config = BackfillConfig {
            enabled: true,
            exit_after_backfill: false,
            rpc_url: self.backfill_config_base.rpc_url.clone(),
            batch_size: self.backfill_config_base.batch_size,
            max_gap_slots: u64::MAX, // No limit for full resync
            start_slot: Some(genesis_slot),
        };

        let backfill_service = BackfillService::new(
            self.storage.clone(),
            self.rpc_poller.clone(),
            self.program_type,
            backfill_config,
            self.escrow_instance_id,
        );

        // Step 4: Setup processing pipeline
        // Create channels for instruction flow and checkpoint updates
        let (instruction_tx, instruction_rx) = mpsc::channel(1000);
        let (checkpoint_tx, checkpoint_rx) = mpsc::channel(1000);

        // Start checkpoint writer service
        let checkpoint_writer = CheckpointWriter::new(self.storage.clone());
        let checkpoint_handle = checkpoint_writer.start(checkpoint_rx);
        info!("CheckpointWriter service started");

        // Start transaction processor as separate tokio task
        let mut transaction_processor =
            TransactionProcessor::new(self.storage.clone(), checkpoint_tx.clone());
        // Wire the escrow instance scope. Config validation guarantees Some for the
        // Escrow program; None here means the Withdraw program, where no instance
        // scoping applies.
        if let Some(instance_id) = self.escrow_instance_id {
            transaction_processor = transaction_processor.with_escrow_instance_id(instance_id);
        }
        // Inject the pre-drop consumed-set so the rebuild reconciles each row in place.
        if let Some(consumed) = consumed {
            transaction_processor = transaction_processor.with_consumed_set(consumed);
        }
        let processor_handle =
            tokio::spawn(async move { transaction_processor.start(instruction_rx).await });
        info!("TransactionProcessor task spawned");

        let total_slots = current_slot.saturating_sub(genesis_slot);

        info!(
            "Starting backfill from slot {} to slot {} ({} slots to process)...",
            genesis_slot, current_slot, total_slots
        );

        // Losing the lock mid-rebuild means a worker can now start against a database
        // that is only half rebuilt, so stop filling rather than race it. The rebuild is
        // repeatable, and the next run starts from a lock it actually holds.
        tokio::select! {
            biased;
            _ = lock_lost.cancelled() => {
                error!("Live-state lock lost during the rebuild; aborting the backfill");
                // Both writers are stopped outright rather than drained. Closing the
                // checkpoint channel is the writer's cue to flush what it has, which
                // would commit a durable frontier over a database this run only half
                // rebuilt and leave no gap for a later run to detect.
                processor_handle.abort();
                checkpoint_handle.abort();
                return Err(IndexerError::Storage(StorageError::LiveStateLockLost));
            }
            result = backfill_service.run(instruction_tx.clone()) => {
                result.map_err(|e| {
                    error!(
                        "Backfill service failed during resync from slot {} to {}: {}",
                        genesis_slot, current_slot, e
                    );
                    e
                })?;
            }
        }
        info!("Backfill service completed");

        // Drop instruction_tx to signal no more instructions coming
        drop(instruction_tx);

        // Wait for processor to finish processing all instructions
        match processor_handle.await {
            Ok(Ok(())) => info!("Transaction processor completed successfully"),
            Ok(Err(e)) => {
                error!("Transaction processor failed during resync: {}", e);
                return Err(e);
            }
            Err(e) => {
                error!("Transaction processor task panicked during resync: {:?}", e);
                return Err(IndexerError::ShutdownChannelSend);
            }
        }

        // Perform cleanup after backfill, with no completeness target to check. A rebuild
        // resolves its range inside the backfill service and never surfaces the top slot,
        // so there is nothing to compare against here. Leaving it unchecked is acceptable
        // because a stale checkpoint after a rebuild heals itself: the next live start
        // detects the gap below the tip and fills it.
        if let Err(e) = crate::shutdown_utils::cleanup_after_backfill(
            checkpoint_handle,
            checkpoint_tx,
            self.storage.clone(),
            None,
        )
        .await
        {
            error!("Cleanup after resync backfill failed: {}", e);
            // Returned as-is so the operator sees which stage failed, not a generic one.
            return Err(e);
        }

        info!(
            "Resync complete for {:?}. Processed {} slots (from {} to {})",
            self.program_type, total_slots, genesis_slot, current_slot
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BackfillConfig, ProgramType};
    use crate::indexer::datasource::rpc_polling::rpc::RpcPoller;
    use crate::storage::common::storage::mock::MockStorage;
    use crate::storage::Storage;
    use solana_sdk::commitment_config::CommitmentLevel;
    use solana_transaction_status::UiTransactionEncoding;
    use std::sync::Arc;

    #[test]
    fn resync_service_new_with_escrow_instance_id() {
        use solana_sdk::pubkey::Pubkey;
        let storage = Arc::new(Storage::Mock(MockStorage::new()));
        let rpc_poller = Arc::new(RpcPoller::new(
            "http://localhost:8899".to_string(),
            UiTransactionEncoding::Json,
            CommitmentLevel::Finalized,
        ));
        let backfill_config = BackfillConfig {
            enabled: false,
            exit_after_backfill: false,
            rpc_url: "http://localhost:8899".to_string(),
            batch_size: 50,
            max_gap_slots: 500,
            start_slot: Some(1000),
        };
        let instance_id = Pubkey::new_unique();

        let service = ResyncService::new(
            storage,
            rpc_poller,
            ProgramType::Withdraw,
            backfill_config,
            Some(instance_id),
        );

        assert_eq!(service.program_type, ProgramType::Withdraw);
        assert_eq!(service.escrow_instance_id, Some(instance_id));
        assert_eq!(service.backfill_config_base.start_slot, Some(1000));
    }

    /// An escrow rebuild with no instance scope would drop every table and refill them with
    /// nothing, so it must abort before the drop rather than after it.
    ///
    /// The RPC points at a dead port, which is what makes the ordering observable: the tip
    /// fetch sits between this guard and the drop, so an `InvalidPubkey` here can only mean
    /// the guard ran first. Had it run later, the unreachable node would have produced a
    /// datasource error instead, and the tables would already be gone.
    #[tokio::test]
    async fn run_refuses_escrow_resync_without_instance_id_before_dropping_tables() {
        let storage = Arc::new(Storage::Mock(MockStorage::new()));
        let rpc_poller = Arc::new(RpcPoller::new(
            "http://127.0.0.1:1".to_string(),
            UiTransactionEncoding::Json,
            CommitmentLevel::Finalized,
        ));
        let backfill_config = BackfillConfig {
            enabled: true,
            exit_after_backfill: false,
            rpc_url: "http://127.0.0.1:1".to_string(),
            batch_size: 50,
            max_gap_slots: 500,
            start_slot: None,
        };

        let service = ResyncService::new(
            storage,
            rpc_poller,
            ProgramType::Escrow,
            backfill_config,
            None,
        );

        match service.run(100).await {
            Err(IndexerError::Reconciliation(ReconciliationError::InvalidPubkey {
                reason,
                ..
            })) => assert!(
                reason.contains("escrow_instance_id"),
                "reason must name the missing scope, got: {reason}"
            ),
            other => panic!("escrow resync with no instance id must fail closed, got: {other:?}"),
        }
    }

    /// A withdraw service on a mock store, with the RPC pointed at a dead port.
    ///
    /// The dead port is what makes ordering observable. The chain-tip fetch sits
    /// between the halt check and the drop, so any error that is not a datasource
    /// error proves the halt check ran first and nothing was destroyed.
    fn halt_test_service(storage: Arc<Storage>) -> ResyncService {
        let rpc_poller = Arc::new(RpcPoller::new(
            "http://127.0.0.1:1".to_string(),
            UiTransactionEncoding::Json,
            CommitmentLevel::Finalized,
        ));
        let backfill_config = BackfillConfig {
            enabled: true,
            exit_after_backfill: false,
            rpc_url: "http://127.0.0.1:1".to_string(),
            batch_size: 50,
            max_gap_slots: 500,
            start_slot: None,
        };
        ResyncService::new(
            storage,
            rpc_poller,
            ProgramType::Withdraw,
            backfill_config,
            None,
        )
    }

    /// A reconciliation halt is a solvency interlock, and a rebuild would drop the
    /// table holding it. Refuse before the drop so the evidence survives.
    #[tokio::test]
    async fn run_refuses_when_reconciliation_halt_is_set() {
        let mock = MockStorage::new();
        mock.set_reconciliation_halt("supply above custody")
            .await
            .unwrap();
        let storage = Arc::new(Storage::Mock(mock));

        match halt_test_service(storage.clone()).run(100).await {
            Err(IndexerError::Reconciliation(ReconciliationError::ReconciliationHalted {
                reason,
            })) => assert!(
                reason.contains("supply above custody"),
                "the refusal must carry the halt reason, got: {reason}"
            ),
            other => panic!("a halted database must refuse to resync, got: {other:?}"),
        }
        match storage.as_ref() {
            Storage::Mock(mock) => assert_eq!(
                mock.calls("drop_tables"),
                0,
                "the refusal must land before any destruction"
            ),
            _ => unreachable!(),
        }
    }

    /// An unreadable halt flag is not proof there is no halt, so it must stop the
    /// rebuild too rather than destroy state it could not check.
    #[tokio::test]
    async fn run_aborts_when_the_halt_flag_is_unreadable() {
        let mock = MockStorage::new();
        mock.set_should_fail("is_reconciliation_halted", true);
        let storage = Arc::new(Storage::Mock(mock));

        match halt_test_service(storage.clone()).run(100).await {
            Err(IndexerError::Storage(_)) => {}
            other => panic!("an unreadable halt flag must fail closed, got: {other:?}"),
        }
        match storage.as_ref() {
            Storage::Mock(mock) => assert_eq!(
                mock.calls("drop_tables"),
                0,
                "the refusal must land before any destruction"
            ),
            _ => unreachable!(),
        }
    }
}
