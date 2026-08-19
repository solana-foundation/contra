use crate::config::ProgramType;
use crate::error::{DataSourceError, IndexerError, ReconciliationError};
use crate::{
    indexer::{
        checkpoint::CheckpointWriter, datasource::common::datasource::DataSource,
        reconciliation::run_startup_reconciliation, transaction_processor::TransactionProcessor,
    },
    shutdown_utils::{cleanup_after_backfill, shutdown_indexer},
    storage::{PostgresDb, Storage},
    DatasourceType, IndexerConfig, PrivateChannelIndexerConfig, StorageType,
};

#[cfg(feature = "datasource-rpc")]
use crate::indexer::backfill::BackfillService;

#[cfg(all(feature = "datasource-rpc", feature = "datasource-yellowstone"))]
use crate::indexer::backfill::ensure_startup_anchor;

#[cfg(feature = "datasource-rpc")]
use crate::indexer::datasource::rpc_polling::{rpc::RpcPoller, RpcPollingSource};

#[cfg(feature = "datasource-yellowstone")]
use crate::indexer::datasource::yellowstone::YellowstoneSource;
use private_channel_metrics::HealthState;
#[cfg(feature = "datasource-rpc")]
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;
use tokio::signal;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

/// Buffer depth for both pipeline channels, shared so the two creation sites cannot drift.
const PIPELINE_CHANNEL_CAPACITY: usize = 1000;

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

/// Run a one-shot backfill and exit, with no live datasource behind it.
///
/// This owns the whole short-lived pipeline. The processor is the only component that
/// writes rows and the only source of checkpoint updates, so it has to be running before
/// the fill starts: with nothing draining the instruction channel the fill either fills
/// the buffer and parks forever or finishes and has its whole output dropped unread.
#[cfg(feature = "datasource-rpc")]
async fn run_backfill_only(
    backfill_service: BackfillService,
    storage: Arc<Storage>,
    program_type: ProgramType,
    escrow_instance_id: Option<Pubkey>,
) -> Result<(), IndexerError> {
    // Resolve first and fail closed: with no live stream there is no ungated fallback.
    let range = backfill_service.resolve_range().await?;

    let (instruction_tx, instruction_rx) = mpsc::channel(PIPELINE_CHANNEL_CAPACITY);
    let (checkpoint_tx, checkpoint_rx) = mpsc::channel(PIPELINE_CHANNEL_CAPACITY);

    // Gating to the fill range keeps a failed slot from being leapfrogged by a later one.
    let mut checkpoint_writer = CheckpointWriter::new(storage.clone());
    if let Some((from_slot, target)) = range.gap {
        checkpoint_writer = checkpoint_writer.with_gate(from_slot, target);
    }
    let checkpoint_handle = checkpoint_writer.start(checkpoint_rx);
    info!("CheckpointWriter service started");

    let mut transaction_processor =
        TransactionProcessor::new(storage.clone(), checkpoint_tx.clone());
    // An unset instance scope makes the processor drop every escrow instruction.
    if let Some(instance_id) = escrow_instance_id {
        transaction_processor = transaction_processor.with_escrow_instance_id(instance_id);
    }
    // Health is deliberately left unwired. The indexer health contract demands continuous
    // progress on a 30 second window, which fits a live stream but not a one-shot job:
    // an ordinary slow stretch here, such as a block fetch riding out its retries, would
    // report the process unhealthy and invite a supervisor to restart a repair that is
    // still making progress. A run that never reports progress stays healthy instead.
    let processor_handle = tokio::spawn(transaction_processor.start(instruction_rx));
    info!("TransactionProcessor task spawned");

    // Held, not propagated, so the drain below still runs and a partial fill keeps its slots.
    let fill_result = match range.gap {
        Some((from_slot, target)) => {
            backfill_service
                .run_range(from_slot, target, instruction_tx.clone())
                .await
        }
        None => {
            info!("No backfill gap to fill");
            Ok(())
        }
    };

    // Releasing the last sender is what ends the processor's receive loop.
    drop(instruction_tx);
    let processor_result = match processor_handle.await {
        Ok(result) => result,
        Err(join_err) => {
            error!("TransactionProcessor task panicked: {:?}", join_err);
            Err(IndexerError::ProcessorPanicked)
        }
    };

    info!("Backfill completed, performing graceful cleanup...");
    // The processor is joined above rather than here for a reason worth stating: it holds
    // a clone of the checkpoint sender, so the writer cannot see its channel close while
    // the processor is alive. Draining first would burn the full drain timeout and then
    // flush a frontier missing every slot the processor had not finished writing yet.
    let cleanup_result = cleanup_after_backfill(
        checkpoint_handle,
        checkpoint_tx,
        storage,
        range.gap.map(|(_, target)| (program_type, target)),
    )
    .await;

    // Order of reporting matters because these failures cause one another. A processor
    // that gives up on a write drops the instruction receiver, which the fill then sees
    // as a send failure, and both leave the checkpoint short of its target. Reporting the
    // processor first names the database error that actually started it, instead of
    // pointing an operator at the channel or at the completeness check downstream of it.
    processor_result?;
    fill_result?;
    cleanup_result
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

    // 2. Startup reconciliation (escrow only, before any data processing).
    //
    // Skip when running in backfill-only mode (backfill.enabled &&
    // backfill.exit_after_backfill). In that mode the DB is intentionally
    // incomplete — reconciling it against the current on-chain state would
    // produce false positives and block the very operation that repairs the
    // discrepancy. Concurrent backfill (exit_after_backfill = false) still
    // runs reconciliation because the live datasource is about to start.
    let backfill_only =
        indexer_config.backfill.enabled && indexer_config.backfill.exit_after_backfill;
    // Checked in every mode, not just the reconciling one. The instance scope is what
    // the processor filters escrow instructions by, and an unset scope drops all of
    // them, so a backfill without it would record nothing while still marking the range
    // done. Refusing to start is the only outcome that leaves the gap repairable.
    if common_config.program_type == ProgramType::Escrow
        && common_config.escrow_instance_id.is_none()
    {
        return Err(IndexerError::Reconciliation(
            ReconciliationError::InvalidPubkey {
                pubkey: "<missing>".to_string(),
                reason: "escrow_instance_id is required for the escrow indexer".to_string(),
            },
        ));
    }

    if !backfill_only {
        match (common_config.program_type, common_config.escrow_instance_id) {
            (ProgramType::Escrow, Some(seed)) => {
                run_startup_reconciliation(
                    &indexer_config.reconciliation,
                    common_config.program_type,
                    &storage,
                    &common_config.rpc_url,
                    // For the escrow indexer, source_rpc_url is the channel (gateway)
                    // handle used only for the supply invariant; None skips it.
                    common_config.source_rpc_url.as_deref(),
                    &seed,
                )
                .await?;
            }
            _ => {
                info!("Startup reconciliation skipped (non-escrow program)");
            }
        }
    } else {
        info!("Startup reconciliation skipped (backfill-only mode)");
    }

    // 3. Create channels
    let (instruction_tx, instruction_rx) = mpsc::channel(PIPELINE_CHANNEL_CAPACITY);
    let (checkpoint_tx, checkpoint_rx) = mpsc::channel(PIPELINE_CHANNEL_CAPACITY);

    // 4. Resolve the backfill range, gate the checkpoint writer to it, then start
    //    the writer. Checkpoint updates only begin once the processor starts further
    //    below (step 8), so the gate is always in place before the first update —
    //    no live-tip slot can slip past it and push the checkpoint over the gap.
    let mut checkpoint_writer = CheckpointWriter::new(storage.clone());

    // First slot the live RPC source must request, captured from the backfill range so
    // both producers share one boundary. None when backfill is disabled or resolves no
    // range, in which case step 6 falls back to the configured from_slot.
    #[cfg(feature = "datasource-rpc")]
    let mut rpc_live_start_slot: Option<u64> = None;

    // Floor of the resolved startup range; None makes the anchor fall back to the chain tip.
    #[cfg(all(feature = "datasource-rpc", feature = "datasource-yellowstone"))]
    let mut startup_anchor_hint: Option<u64> = None;

    if indexer_config.backfill.enabled {
        #[cfg(not(feature = "datasource-rpc"))]
        return Err(DataSourceError::InvalidConfig {
            reason: "Datasource rpc needs to be enabled for backfilling".to_string(),
        });

        #[cfg(feature = "datasource-rpc")]
        {
            use crate::error::DataSourceError;

            let rpc_polling_config = indexer_config.rpc_polling.as_ref().ok_or_else(|| {
                DataSourceError::InvalidConfig {
                    reason: "RPC polling config required for backfill".to_string(),
                }
            })?;
            let rpc_poller = Arc::new(RpcPoller::new(
                indexer_config.backfill.rpc_url.clone(),
                rpc_polling_config.encoding,
                rpc_polling_config.commitment,
            ));

            let backfill_service = BackfillService::new(
                storage.clone(),
                rpc_poller,
                common_config.program_type,
                indexer_config.backfill.clone(),
                common_config.escrow_instance_id,
            );

            if indexer_config.backfill.exit_after_backfill {
                return run_backfill_only(
                    backfill_service,
                    storage.clone(),
                    common_config.program_type,
                    common_config.escrow_instance_id,
                )
                .await;
            } else {
                // Gate the writer to the range backfill will fill. resolve_range retries
                // transient RPC failures; a persistent failure fails closed (see below).
                match backfill_service.resolve_range().await {
                    Ok(range) => {
                        // Pin the live source to backfill's boundary so it resumes with
                        // no hole and no overlap, whether or not there is a gap to fill.
                        rpc_live_start_slot = Some(range.live_start_slot);
                        // The floor, never the target: the range above it is not filled yet.
                        #[cfg(feature = "datasource-yellowstone")]
                        {
                            startup_anchor_hint = Some(range.anchor);
                        }
                        if let Some((from_slot, target)) = range.gap {
                            checkpoint_writer = checkpoint_writer.with_gate(from_slot, target);
                            let instruction_tx_clone = instruction_tx.clone();
                            tokio::spawn(async move {
                                if let Err(e) = backfill_service
                                    .run_range(from_slot, target, instruction_tx_clone)
                                    .await
                                {
                                    error!("Backfill failed: {}", e);
                                } else {
                                    info!("Backfill completed successfully");
                                }
                            });
                        } else {
                            info!("No backfill gap; checkpoint writer left ungated");
                        }
                    }
                    Err(e) => {
                        error!(
                            "Backfill range resolution failed after retries; refusing to start \
                             rather than running ungated past the unfilled gap: {}",
                            e
                        );
                        return Err(e);
                    }
                }
            }
        }
    }

    let checkpoint_handle = checkpoint_writer.start(checkpoint_rx);
    info!("CheckpointWriter service started");

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

                source
                    .with_gap_detection(
                        gap_rpc_poller,
                        indexer_config.backfill.max_gap_slots,
                        indexer_config.backfill.batch_size,
                    )
                    .with_storage(storage.clone())
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

    // 8. Start transaction processor
    let mut transaction_processor =
        TransactionProcessor::new(storage.clone(), checkpoint_tx.clone());
    // Wire the escrow instance scope. Config validation guarantees Some for the
    // Escrow program; None here means the Withdraw program, where no instance
    // scoping applies.
    if let Some(instance_id) = common_config.escrow_instance_id {
        transaction_processor = transaction_processor.with_escrow_instance_id(instance_id);
    }
    if let Some(h) = health.clone() {
        transaction_processor = transaction_processor.with_health(h);
    }
    let mut processor_handle = tokio::spawn(transaction_processor.start(instruction_rx));

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

    /// One-shot backfill: every slot recorded, and the checkpoint only reaching the target
    /// once the whole range is durably stored.
    ///
    /// Each case scripts a mock RPC and a mock store, then drives the real pipeline, so
    /// what is under test is the wiring between the fill, the processor and the writer
    /// rather than any one of them in isolation.
    #[cfg(feature = "datasource-rpc")]
    mod backfill_only {
        use super::*;
        use crate::config::BackfillConfig;
        use crate::storage::common::storage::mock::MockStorage;
        use crate::test_utils::rpc_mocks::{
            chain, deposit_fixture_instance, mock_get_block_at, mock_get_block_error,
            mock_get_block_with_deposit, mock_get_blocks, mock_get_blocks_with_limit,
            mock_get_slot,
        };
        use mockito::Server;
        use solana_sdk::commitment_config::CommitmentLevel;
        use solana_transaction_status::UiTransactionEncoding;
        use std::time::Duration;

        /// Store seeded with an escrow checkpoint, plus the handle tests assert against.
        fn seeded_storage(checkpoint: u64) -> (MockStorage, Arc<Storage>) {
            let mock = MockStorage::new();
            mock.set_checkpoint("escrow", checkpoint);
            (mock.clone(), Arc::new(Storage::Mock(mock)))
        }

        /// Escrow backfill service pointed at the mock RPC.
        fn service(
            server: &Server,
            storage: Arc<Storage>,
            batch_size: usize,
            max_gap_slots: u64,
            escrow_instance_id: Option<Pubkey>,
        ) -> BackfillService {
            let poller = Arc::new(RpcPoller::new(
                server.url(),
                UiTransactionEncoding::Json,
                CommitmentLevel::Finalized,
            ));
            BackfillService::new(
                storage,
                poller,
                ProgramType::Escrow,
                BackfillConfig {
                    enabled: true,
                    exit_after_backfill: true,
                    rpc_url: server.url(),
                    batch_size,
                    max_gap_slots,
                    start_slot: None,
                },
                escrow_instance_id,
            )
        }

        /// Escrow checkpoint currently held by the mock store.
        fn checkpoint_of(mock: &MockStorage) -> Option<u64> {
            mock.committed_checkpoints
                .lock()
                .unwrap()
                .get("escrow")
                .copied()
        }

        /// Every slot in the range is consumed, so the checkpoint lands on the target.
        #[tokio::test]
        async fn backfill_only_records_slots_and_advances_checkpoint() {
            let mut server = Server::new_async().await;
            let _slot = mock_get_slot(&mut server, 103);
            let _blocks = chain(&mut server, 101, 103, &[(101, 100), (102, 101), (103, 102)]);
            let (mock, storage) = seeded_storage(100);

            let backfill = service(&server, storage.clone(), 10, 1000, None);
            let result = run_backfill_only(backfill, storage, ProgramType::Escrow, None).await;

            assert!(result.is_ok(), "clean backfill must succeed: {result:?}");
            assert_eq!(
                checkpoint_of(&mock),
                Some(103),
                "the checkpoint must reach the fill target, which only happens if a \
                 processor consumed every SlotComplete"
            );
        }

        /// A range larger than the channel buffer must still drain rather than deadlock.
        #[tokio::test]
        async fn backfill_only_drains_more_slots_than_channel_capacity() {
            let tip = 100 + PIPELINE_CHANNEL_CAPACITY as u64 + 500;
            let mut server = Server::new_async().await;
            let _slot = mock_get_slot(&mut server, tip);
            // No producers plus a witness past the range proves every slot empty in one batch.
            let _blocks = mock_get_blocks(&mut server, 101, tip, &[]);
            let _witness = mock_get_blocks_with_limit(&mut server, tip + 1, &[tip + 1]);
            let _witness_block = mock_get_block_at(&mut server, tip + 1, 100);
            let (mock, storage) = seeded_storage(100);

            let backfill = service(&server, storage.clone(), 10_000, u64::MAX, None);
            let outcome = tokio::time::timeout(
                Duration::from_secs(60),
                run_backfill_only(backfill, storage, ProgramType::Escrow, None),
            )
            .await;

            let result = outcome.expect(
                "a backfill wider than the channel buffer must not park forever waiting \
                 for a consumer",
            );
            assert!(result.is_ok(), "wide backfill must succeed: {result:?}");
            assert_eq!(checkpoint_of(&mock), Some(tip));
        }

        /// Parsed instructions, not just slot markers, have to reach storage.
        #[tokio::test]
        async fn backfill_only_writes_deposit_rows_for_configured_instance() {
            let mut server = Server::new_async().await;
            let _slot = mock_get_slot(&mut server, 101);
            let _blocks = mock_get_blocks(&mut server, 101, 101, &[101]);
            let _block = mock_get_block_with_deposit(&mut server, 101, 100, 4242);
            let (mock, storage) = seeded_storage(100);

            let instance = Some(deposit_fixture_instance());
            let backfill = service(&server, storage.clone(), 10, 1000, instance);
            let result = run_backfill_only(backfill, storage, ProgramType::Escrow, instance).await;

            assert!(result.is_ok(), "deposit backfill must succeed: {result:?}");
            let rows: Vec<_> = mock
                .inserted_transactions
                .lock()
                .unwrap()
                .iter()
                .flatten()
                .cloned()
                .collect();
            assert_eq!(
                rows.len(),
                1,
                "the backfilled deposit must be written; an unscoped processor drops it"
            );
            assert_eq!(rows[0].amount.value(), 4242);
            assert_eq!(checkpoint_of(&mock), Some(101));
        }

        /// A fill that dies part way still persists the contiguous prefix it completed.
        #[tokio::test]
        async fn backfill_only_persists_partial_frontier_when_fill_fails() {
            let mut server = Server::new_async().await;
            let _slot = mock_get_slot(&mut server, 103);
            // batch_size 2 splits the range so the first batch lands before the second fails.
            let _first = mock_get_blocks(&mut server, 101, 102, &[101, 102]);
            let _b1 = mock_get_block_at(&mut server, 101, 100);
            let _b2 = mock_get_block_at(&mut server, 102, 101);
            let _second = mock_get_blocks(&mut server, 103, 103, &[103]);
            let _b3 = mock_get_block_error(&mut server, 103, -32600, "Invalid request");
            let (mock, storage) = seeded_storage(100);

            let backfill = service(&server, storage.clone(), 2, 1000, None);
            let result = run_backfill_only(backfill, storage, ProgramType::Escrow, None).await;

            assert!(result.is_err(), "a failed fetch must fail the run");
            assert_eq!(
                checkpoint_of(&mock),
                Some(102),
                "the slots that were stored must still be checkpointed so a retry resumes"
            );
        }

        /// A checkpoint that never reaches the target must not report success.
        #[tokio::test]
        async fn backfill_only_reports_incomplete_when_checkpoint_flush_fails() {
            let mut server = Server::new_async().await;
            let _slot = mock_get_slot(&mut server, 103);
            let _blocks = chain(&mut server, 101, 103, &[(101, 100), (102, 101), (103, 102)]);
            let (mock, storage) = seeded_storage(100);
            // Every checkpoint write fails; the writer only warns, so the run must catch it.
            mock.set_should_fail("escrow", true);

            let backfill = service(&server, storage.clone(), 10, 1000, None);
            let result = run_backfill_only(backfill, storage, ProgramType::Escrow, None).await;

            match result {
                Err(IndexerError::BackfillIncomplete {
                    committed, target, ..
                }) => {
                    assert_eq!(committed, Some(100));
                    assert_eq!(target, 103);
                }
                other => panic!("a stalled checkpoint must fail the run, got: {other:?}"),
            }
        }

        /// Nothing to fill is a clean exit that touches neither RPC blocks nor the checkpoint.
        #[tokio::test]
        async fn backfill_only_no_gap_is_a_clean_noop() {
            let mut server = Server::new_async().await;
            let _slot = mock_get_slot(&mut server, 100);
            let no_blocks = server
                .mock("POST", "/")
                .match_body(mockito::Matcher::PartialJson(
                    serde_json::json!({ "method": "getBlocks" }),
                ))
                .expect(0)
                .create();
            let (mock, storage) = seeded_storage(100);

            let backfill = service(&server, storage.clone(), 10, 1000, None);
            let result = run_backfill_only(backfill, storage, ProgramType::Escrow, None).await;

            assert!(result.is_ok(), "an empty range must succeed: {result:?}");
            no_blocks.assert();
            assert_eq!(
                checkpoint_of(&mock),
                Some(100),
                "no gap means no slot was processed, so the checkpoint stands still"
            );
        }
    }
}
