use crate::config::{ProgramType, ReconciliationConfig};
use crate::error::{DataSourceError, IndexerError, ReconciliationError};
use crate::{
    indexer::{
        checkpoint::{CheckpointMsg, CheckpointWriter},
        datasource::common::{datasource::DataSource, types::ProcessorMessage},
        reconciliation::run_startup_reconciliation,
        transaction_processor::TransactionProcessor,
    },
    shutdown_utils::{cleanup_after_backfill, shutdown_indexer},
    storage::{PostgresDb, Storage},
    DatasourceType, IndexerConfig, PrivateChannelIndexerConfig, StorageType,
};

#[cfg(feature = "datasource-rpc")]
use crate::{
    channel_utils::send_guaranteed,
    error::BackfillError,
    indexer::{
        backfill::BackfillService,
        checkpoint::{wait_for_checkpoint_commit, CHECKPOINT_COMMIT_TIMEOUT_SECS},
    },
};
#[cfg(feature = "datasource-rpc")]
use std::time::Duration;

#[cfg(all(feature = "datasource-rpc", feature = "datasource-yellowstone"))]
use crate::indexer::backfill::ensure_startup_anchor;

#[cfg(feature = "datasource-rpc")]
use crate::indexer::datasource::rpc_polling::{rpc::RpcPoller, RpcPollingSource};

#[cfg(feature = "datasource-yellowstone")]
use crate::indexer::datasource::yellowstone::YellowstoneSource;
use private_channel_metrics::HealthState;
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;
use tokio::signal;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
#[cfg(feature = "datasource-rpc")]
use tracing::warn;
use tracing::{error, info};

/// Which side of the processor-vs-shutdown race fired.
enum Supervision {
    /// The processor task ended on its own, carrying its join result: a clean
    /// stop, a fatal write-exhaustion error, or a panic.
    ProcessorEnded(Result<Result<(), IndexerError>, tokio::task::JoinError>),
    /// A shutdown signal arrived while the processor was still running.
    ShutdownSignalled(std::io::Result<()>),
}

/// Race the running processor task against the shutdown signal. Biased to the
/// processor so a fatal error that becomes ready at the same moment as the
/// signal still wins, and the caller exits non-zero instead of reporting a
/// clean shutdown.
async fn supervise(
    processor_handle: &mut tokio::task::JoinHandle<Result<(), IndexerError>>,
    shutdown: impl std::future::Future<Output = std::io::Result<()>>,
) -> Supervision {
    tokio::select! {
        biased;
        res = &mut *processor_handle => Supervision::ProcessorEnded(res),
        sig = shutdown => Supervision::ShutdownSignalled(sig),
    }
}

/// Reconcile attempts before a mismatch is treated as real rather than as a deposit that
/// landed while startup was still catching up.
#[cfg(feature = "datasource-rpc")]
const RECONCILE_MAX_ATTEMPTS: u32 = 3;

/// Pause between reconcile attempts, so the next one has new slots to pull in.
#[cfg(all(feature = "datasource-rpc", not(test)))]
const RECONCILE_RETRY_DELAY_MS: u64 = 2_000;
#[cfg(all(feature = "datasource-rpc", test))]
const RECONCILE_RETRY_DELAY_MS: u64 = 10;

/// Whether another fill could plausibly clear this failure.
///
/// Only a custody-versus-ledger mismatch can be, and only because the chain kept moving
/// while startup was catching up, so the next fill may pull in the rows that explain it.
/// Everything else, a supply breach included, compares the same two numbers however many
/// times it runs, so it stops the boot on the spot instead of paying for two more fills.
#[cfg(feature = "datasource-rpc")]
fn reconcile_error_may_clear(error: &IndexerError) -> bool {
    matches!(
        error,
        IndexerError::Reconciliation(ReconciliationError::MismatchExceedsThreshold { .. })
    )
}

/// Compare on-chain escrow custody against the indexed ledger.
///
/// A non-escrow program has no custody to check and returns immediately. The instance id
/// is validated once at startup, so its absence here can only mean a non-escrow program.
async fn reconcile_escrow(
    config: &ReconciliationConfig,
    common_config: &PrivateChannelIndexerConfig,
    storage: &Arc<Storage>,
) -> Result<(), IndexerError> {
    let Some(instance_id) = common_config.escrow_instance_id else {
        return Ok(());
    };

    run_startup_reconciliation(
        config,
        common_config.program_type,
        storage,
        &common_config.rpc_url,
        // For the escrow indexer, source_rpc_url is the channel (gateway) handle used
        // only for the supply invariant; None skips it.
        common_config.source_rpc_url.as_deref(),
        &instance_id,
    )
    .await
}

/// Build the service that resolves and fills the startup range.
#[cfg(feature = "datasource-rpc")]
fn build_backfill_service(
    storage: Arc<Storage>,
    common_config: &PrivateChannelIndexerConfig,
    indexer_config: &IndexerConfig,
) -> Result<BackfillService, IndexerError> {
    let rpc_polling_config =
        indexer_config
            .rpc_polling
            .as_ref()
            .ok_or_else(|| DataSourceError::InvalidConfig {
                reason: "RPC polling config required for backfill".to_string(),
            })?;

    let rpc_poller = Arc::new(RpcPoller::new(
        indexer_config.backfill.rpc_url.clone(),
        rpc_polling_config.encoding,
        rpc_polling_config.commitment,
    ));

    Ok(BackfillService::new(
        storage,
        rpc_poller,
        common_config.program_type,
        indexer_config.backfill.clone(),
        common_config.escrow_instance_id,
    ))
}

/// Spawn the processor that turns decoded instructions into rows and checkpoint updates.
fn spawn_transaction_processor(
    storage: Arc<Storage>,
    checkpoint_tx: mpsc::Sender<CheckpointMsg>,
    instruction_rx: mpsc::Receiver<ProcessorMessage>,
    escrow_instance_id: Option<Pubkey>,
    health: Option<Arc<HealthState>>,
) -> tokio::task::JoinHandle<Result<(), IndexerError>> {
    let mut transaction_processor = TransactionProcessor::new(storage, checkpoint_tx);
    // Wire the escrow instance scope. Config validation guarantees Some for the
    // Escrow program; None here means the Withdraw program, where no instance
    // scoping applies.
    if let Some(instance_id) = escrow_instance_id {
        transaction_processor = transaction_processor.with_escrow_instance_id(instance_id);
    }
    if let Some(h) = health {
        transaction_processor = transaction_processor.with_health(h);
    }
    tokio::spawn(transaction_processor.start(instruction_rx))
}

pub async fn run(
    common_config: PrivateChannelIndexerConfig,
    indexer_config: IndexerConfig,
    health: Option<Arc<HealthState>>,
) -> Result<(), IndexerError> {
    info!("Starting PrivateChannel Indexer");
    info!("Program: {:?}", common_config.program_type);
    info!("Datasource: {:?}", indexer_config.datasource_type);
    info!("Storage: {:?}", common_config.storage_type);
    info!("RPC URL: {}", common_config.rpc_url);
    info!("Backfill enabled: {}", indexer_config.backfill.enabled);

    // 1. Initialize storage
    let storage: Arc<Storage> = match common_config.storage_type {
        StorageType::Postgres => Arc::new(Storage::Postgres(
            PostgresDb::new(&common_config.postgres)
                .await
                .map_err(|e| IndexerError::Storage(e.into()))?,
        )),
    };
    storage.init_schema().await?;
    info!("Storage initialized");

    // 2. Validate the escrow reconciliation wiring before doing any work.
    //
    // Only the config check runs here. The reconciliation itself compares on-chain
    // custody against the database, so it has to wait until backfill has finished
    // importing whatever the database is missing; running it first compares live
    // custody against a ledger that is knowingly stale. This check has no such
    // dependency, so keeping it here makes a misconfiguration fail in milliseconds
    // instead of after a full backfill.
    let backfill_only =
        indexer_config.backfill.enabled && indexer_config.backfill.exit_after_backfill;
    match (common_config.program_type, common_config.escrow_instance_id) {
        (ProgramType::Escrow, None) => {
            return Err(IndexerError::Reconciliation(
                ReconciliationError::InvalidPubkey {
                    pubkey: "<missing>".to_string(),
                    reason: "escrow_instance_id is required for escrow reconciliation".to_string(),
                },
            ));
        }
        (ProgramType::Escrow, Some(_)) => {}
        _ => {
            info!("Startup reconciliation skipped (non-escrow program)");
        }
    }

    if backfill_only {
        info!("Startup reconciliation skipped (backfill-only mode)");
    } else if !indexer_config.backfill.enabled {
        // No import is configured, so the ledger will not get any more complete than it
        // is right now and the comparison is as meaningful here as anywhere.
        reconcile_escrow(&indexer_config.reconciliation, &common_config, &storage).await?;
    }

    // 3. Create channels
    let (instruction_tx, instruction_rx) = mpsc::channel(1000);
    let (checkpoint_tx, checkpoint_rx) = mpsc::channel(1000);

    // 4a. Backfill-only mode is self-contained: it gates the writer to the fill range,
    //     runs the fill and exits. Nothing below this point applies to it.
    if backfill_only {
        #[cfg(not(feature = "datasource-rpc"))]
        return Err(DataSourceError::InvalidConfig {
            reason: "Datasource rpc needs to be enabled for backfilling".to_string(),
        }
        .into());

        #[cfg(feature = "datasource-rpc")]
        {
            let backfill_service =
                build_backfill_service(storage.clone(), &common_config, &indexer_config)?;

            // Gate the writer to the fill range so a withheld (failed-write) slot stalls
            // the checkpoint instead of being leapfrogged by a later one. No live stream,
            // so a resolve failure fails closed rather than falling back to ungated.
            let mut checkpoint_writer = CheckpointWriter::new(storage.clone());
            let range = backfill_service.resolve_range().await?;
            if let Some((from_slot, target)) = range.gap {
                checkpoint_writer = checkpoint_writer.with_gate(from_slot, target);
            }
            let checkpoint_handle = checkpoint_writer.start(checkpoint_rx);
            info!("CheckpointWriter service started");
            if let Some((from_slot, target)) = range.gap {
                backfill_service
                    .run_range(from_slot, target, instruction_tx.clone())
                    .await?;
            }
            info!("Backfill completed, performing graceful cleanup...");
            if let Err(e) = cleanup_after_backfill(checkpoint_handle, checkpoint_tx, storage).await
            {
                error!("Cleanup after backfill failed: {}", e);
                return Err(IndexerError::ShutdownChannelSend);
            }
            return Ok(());
        }
    }

    // 4b. Start the checkpoint writer ungated. When a fill runs below it arms the gate
    //     in-band with a Regate that rides ahead of the slots it protects, which also
    //     lets a second attempt re-arm over a range the first one did not cover.
    let checkpoint_handle = CheckpointWriter::new(storage.clone()).start(checkpoint_rx);
    info!("CheckpointWriter service started");

    // 4c. Start the processor before any fill, because the fill blocks on a full
    //     instruction channel and nothing else drains it. Until the datasource starts
    //     the processor simply parks waiting for messages.
    let mut processor_handle = spawn_transaction_processor(
        storage.clone(),
        checkpoint_tx.clone(),
        instruction_rx,
        common_config.escrow_instance_id,
        health.clone(),
    );

    // First slot the live RPC source must request, captured from the backfill range so
    // both producers share one boundary. None when backfill is disabled or resolves no
    // range, in which case the datasource falls back to the configured from_slot.
    #[cfg(feature = "datasource-rpc")]
    let mut rpc_live_start_slot: Option<u64> = None;

    // Floor of the resolved startup range; None makes the anchor fall back to the chain tip.
    #[cfg(all(feature = "datasource-rpc", feature = "datasource-yellowstone"))]
    let mut startup_anchor_hint: Option<u64> = None;

    // Whether a startup fill ran ahead of the live stream, which is what opens the window
    // the Yellowstone source has to repair on its very first connection.
    #[cfg(all(feature = "datasource-rpc", feature = "datasource-yellowstone"))]
    let mut backfill_preceded_stream = false;

    // 4d. Fill the missing range, wait for it to become durable, then reconcile.
    //
    // Reconciliation compares current on-chain custody against the indexed ledger, so it
    // can only be trusted once the ledger has caught up. Waiting for the checkpoint, not
    // merely for the fill to return, is what makes "caught up" verifiable: the gate only
    // lets the checkpoint reach the target after every slot below it has been written.
    //
    // The loop exists because the chain keeps moving. A deposit landing between the range
    // being resolved and custody being read is on-chain but not yet indexed, and at the
    // default zero tolerance that is fatal. Each retry re-fills only the slots the
    // previous attempt took to run, so the window shrinks quickly. A mismatch that
    // backfilling cannot explain still fails the boot on the final attempt.
    if indexer_config.backfill.enabled {
        #[cfg(not(feature = "datasource-rpc"))]
        return Err(DataSourceError::InvalidConfig {
            reason: "Datasource rpc needs to be enabled for backfilling".to_string(),
        }
        .into());

        #[cfg(feature = "datasource-rpc")]
        {
            let backfill_service =
                build_backfill_service(storage.clone(), &common_config, &indexer_config)?;

            for attempt in 1..=RECONCILE_MAX_ATTEMPTS {
                let range = match backfill_service.resolve_range().await {
                    Ok(range) => range,
                    Err(e) => {
                        error!(
                            "Backfill range resolution failed after retries; refusing to start \
                             rather than running ungated past the unfilled gap: {}",
                            e
                        );
                        return Err(e);
                    }
                };

                // Pin the live source to backfill's boundary so it resumes with no hole
                // and no overlap. Taken from the last attempt, which resolved highest.
                rpc_live_start_slot = Some(range.live_start_slot);
                // The floor, never the target: the range above it is not filled yet.
                #[cfg(feature = "datasource-yellowstone")]
                {
                    startup_anchor_hint = Some(range.anchor);
                }

                if let Some((from_slot, target)) = range.gap {
                    send_guaranteed(
                        &instruction_tx,
                        ProcessorMessage::Regate {
                            program_type: common_config.program_type,
                            from: from_slot,
                            target,
                        },
                        "Regate (startup backfill)",
                    )
                    .await
                    .map_err(|e| IndexerError::Backfill(BackfillError::ChannelSend(e)))?;

                    backfill_service
                        .run_range(from_slot, target, instruction_tx.clone())
                        .await?;

                    wait_for_checkpoint_commit(
                        &storage,
                        common_config.program_type,
                        target,
                        Duration::from_secs(CHECKPOINT_COMMIT_TIMEOUT_SECS),
                    )
                    .await?;
                    info!("Backfill completed successfully");
                } else {
                    info!("No backfill gap; checkpoint writer left ungated");
                }

                match reconcile_escrow(&indexer_config.reconciliation, &common_config, &storage)
                    .await
                {
                    Ok(()) => break,
                    Err(e) if attempt < RECONCILE_MAX_ATTEMPTS && reconcile_error_may_clear(&e) => {
                        warn!(
                            "Startup reconciliation attempt {}/{} did not balance, retrying \
                             after letting the chain move on: {}",
                            attempt, RECONCILE_MAX_ATTEMPTS, e
                        );
                        // A retry is only worth anything once there are new slots to pull
                        // in. Reconciling again against a range that has not moved would
                        // compare the same two numbers and burn the attempt for nothing.
                        tokio::time::sleep(Duration::from_millis(RECONCILE_RETRY_DELAY_MS)).await;
                    }
                    Err(e) => return Err(e),
                }
            }

            #[cfg(feature = "datasource-yellowstone")]
            {
                backfill_preceded_stream = true;
            }
        }
    }

    // 6. Start datasource
    let mut datasource: Box<dyn DataSource> = match indexer_config.datasource_type {
        #[cfg(feature = "datasource-rpc")]
        DatasourceType::RpcPolling => {
            let rpc_config = indexer_config.rpc_polling.as_ref().ok_or_else(|| {
                DataSourceError::InvalidConfig {
                    reason: "RPC polling config required for RpcPolling datasource".to_string(),
                }
            })?;

            let mut source = RpcPollingSource::new(
                common_config.rpc_url.clone(),
                // Resume on backfill's boundary when it ran; otherwise the configured start.
                rpc_live_start_slot.or(rpc_config.from_slot),
                rpc_config.poll_interval_ms,
                rpc_config.error_retry_interval_ms,
                rpc_config.batch_size,
                rpc_config.encoding,
                rpc_config.commitment,
                common_config.program_type,
                common_config.escrow_instance_id,
                common_config.fallback_rpc_url.clone(),
            );
            if let Some(h) = health.clone() {
                source = source.with_health(h);
            }
            Box::new(source)
        }

        #[cfg(feature = "datasource-yellowstone")]
        DatasourceType::Yellowstone => {
            let yellowstone_config = indexer_config.yellowstone.as_ref().ok_or_else(|| {
                DataSourceError::InvalidConfig {
                    reason: "Yellowstone config required for Yellowstone datasource".to_string(),
                }
            })?;

            info!(
                "Starting Yellowstone datasource from {} (commitment: {})",
                yellowstone_config.endpoint, yellowstone_config.commitment
            );

            let source = YellowstoneSource::new(
                yellowstone_config.endpoint.clone(),
                yellowstone_config.x_token.clone(),
                yellowstone_config.commitment.clone(),
                common_config.program_type,
                common_config.escrow_instance_id,
            );

            #[cfg(feature = "datasource-rpc")]
            let source = {
                use solana_sdk::commitment_config::CommitmentLevel as SdkCommitmentLevel;
                use solana_transaction_status::UiTransactionEncoding;

                let encoding = indexer_config
                    .rpc_polling
                    .as_ref()
                    .map(|c| c.encoding)
                    .unwrap_or(UiTransactionEncoding::Json);

                let commitment = match yellowstone_config.commitment.to_lowercase().as_str() {
                    "processed" => SdkCommitmentLevel::Processed,
                    "finalized" => SdkCommitmentLevel::Finalized,
                    _ => SdkCommitmentLevel::Confirmed,
                };

                let gap_rpc_poller = Arc::new(RpcPoller::new(
                    indexer_config.backfill.rpc_url.clone(),
                    encoding,
                    commitment,
                ));

                info!(
                    "Yellowstone gap detection enabled (max_gap: {}, batch_size: {})",
                    indexer_config.backfill.max_gap_slots, indexer_config.backfill.batch_size
                );

                // Reconnect repair replays from the durable checkpoint, so one has to exist
                // before the stream can deliver anything. Failing here refuses to start,
                // which beats streaming past a window that could never be recovered: once a
                // later slot is checkpointed, the slots below it stop being reachable.
                ensure_startup_anchor(
                    &storage,
                    common_config.program_type,
                    &gap_rpc_poller,
                    startup_anchor_hint,
                )
                .await?;

                let source = source
                    .with_gap_detection(
                        gap_rpc_poller,
                        indexer_config.backfill.max_gap_slots,
                        indexer_config.backfill.batch_size,
                    )
                    .with_storage(storage.clone());

                // A fill that ran to completion above leaves the slots produced since its
                // target covered by neither it nor the stream, and the first streamed slot
                // would carry the checkpoint straight over them. Arming the first
                // connection replays that window instead. The anchor it needs was just
                // guaranteed above.
                if backfill_preceded_stream {
                    source.with_first_connection_arming()
                } else {
                    source
                }
            };

            let source = if let Some(h) = health.clone() {
                source.with_health(h)
            } else {
                source
            };

            Box::new(source)
        }

        // Catch-all for disabled features
        #[allow(unreachable_patterns)]
        _ => {
            return Err(DataSourceError::InvalidConfig {
                reason: format!(
                    "Datasource {:?} is not compiled. Rebuild with the appropriate feature flag",
                    indexer_config.datasource_type
                ),
            }
            .into());
        }
    };

    // 7. Create cancellation token for graceful shutdown
    let cancellation_token = CancellationToken::new();

    info!("Starting datasource...");
    let datasource_handle = datasource
        .start(instruction_tx.clone(), cancellation_token.clone())
        .await?;

    info!("Indexer started, waiting for shutdown signal...");

    // 9. Race the processor against the shutdown signal. The processor never
    // returns on its own during normal operation (instruction_tx is held here
    // and by the datasource), so the processor side only fires on a fatal write
    // failure or a panic - both must crash the process so the supervisor
    // restarts it and the failed slot replays from the durable checkpoint.
    match supervise(&mut processor_handle, signal::ctrl_c()).await {
        Supervision::ProcessorEnded(res) => {
            // Flush batched checkpoints for already-committed slots so a restart resumes
            // from the latest durable point; timeout-bounded since a dead DB would stall it.
            cancellation_token.cancel();
            drop(instruction_tx);
            drop(checkpoint_tx);
            let _ =
                tokio::time::timeout(std::time::Duration::from_secs(5), checkpoint_handle).await;

            match res {
                Ok(Ok(())) => {
                    info!("TransactionProcessor stopped cleanly");
                }
                Ok(Err(e)) => {
                    error!("TransactionProcessor failed fatally: {}", e);
                    return Err(e);
                }
                Err(join_err) => {
                    error!("TransactionProcessor task panicked: {:?}", join_err);
                    return Err(IndexerError::ProcessorPanicked);
                }
            }
        }
        Supervision::ShutdownSignalled(signal_res) => {
            signal_res.map_err(|_| IndexerError::ShutdownChannelSend)?;
            info!("Shutdown signal received, initiating graceful shutdown...");

            // 10. Graceful shutdown
            shutdown_indexer(
                cancellation_token,
                storage,
                datasource,
                datasource_handle,
                instruction_tx,
                checkpoint_tx,
                checkpoint_handle,
                processor_handle,
            )
            .await
            .map_err(|_| IndexerError::ShutdownChannelSend)?;
        }
    }

    info!("Indexer shutdown complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a balance mismatch earns another fill. Retrying the rest would repeat the
    /// whole range resolution and fill twice over before failing with the same error,
    /// and would log an infrastructure fault as if the books were out by a deposit.
    #[cfg(feature = "datasource-rpc")]
    #[test]
    fn only_a_balance_mismatch_is_worth_another_fill() {
        let cases = [
            (
                ReconciliationError::MismatchExceedsThreshold {
                    count: 1,
                    threshold: 0,
                },
                true,
            ),
            (
                ReconciliationError::Rpc {
                    mint: "mint".to_string(),
                    reason: "unreachable".to_string(),
                },
                false,
            ),
            (
                ReconciliationError::SupplyExceedsCustody {
                    count: 1,
                    threshold: 0,
                },
                false,
            ),
            (ReconciliationError::MissingChannelRpc, false),
            (
                ReconciliationError::InvalidPubkey {
                    pubkey: "bad".to_string(),
                    reason: "malformed".to_string(),
                },
                false,
            ),
            (
                ReconciliationError::DbBalanceOverflow {
                    mint: "mint".to_string(),
                    net: "1".to_string(),
                },
                false,
            ),
        ];

        for (error, expected) in cases {
            let rendered = error.to_string();
            assert_eq!(
                reconcile_error_may_clear(&IndexerError::Reconciliation(error)),
                expected,
                "wrong retry decision for: {rendered}"
            );
        }
    }

    /// A ready shutdown future must not steal the race from an already-finished
    /// processor: the biased select reports the processor's fatal error so run()
    /// exits non-zero rather than treating it as a clean shutdown.
    #[tokio::test]
    async fn supervise_prefers_finished_processor_over_ready_signal() {
        let mut handle = tokio::spawn(async { Err(IndexerError::CheckpointChannelClosed) });
        // Let the task run to completion so its future is ready when raced.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let outcome = supervise(&mut handle, std::future::ready(Ok(()))).await;

        match outcome {
            Supervision::ProcessorEnded(Ok(Err(IndexerError::CheckpointChannelClosed))) => {}
            _ => panic!("biased select must report the finished processor's fatal error"),
        }
    }

    /// While the processor is still running, a ready shutdown signal wins.
    #[tokio::test]
    async fn supervise_takes_shutdown_when_processor_running() {
        let mut handle = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            Ok(())
        });

        let outcome = supervise(&mut handle, std::future::ready(Ok(()))).await;

        assert!(matches!(outcome, Supervision::ShutdownSignalled(Ok(()))));
        handle.abort();
    }

    /// A processor panic surfaces as a join error so run() maps it to a fatal
    /// ProcessorPanicked exit rather than a clean shutdown.
    #[tokio::test]
    async fn supervise_surfaces_processor_panic() {
        let mut handle: tokio::task::JoinHandle<Result<(), IndexerError>> =
            tokio::spawn(async { panic!("processor boom") });
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let outcome = supervise(&mut handle, std::future::pending::<std::io::Result<()>>()).await;

        assert!(matches!(outcome, Supervision::ProcessorEnded(Err(_))));
    }
}
