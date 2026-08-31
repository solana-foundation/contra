use crate::config::{ProgramType, ReconciliationConfig};
use crate::error::{DataSourceError, IndexerError, ReconciliationError};
use crate::{
    indexer::{
        checkpoint::{CheckpointMsg, CheckpointWriter},
        datasource::common::{datasource::DataSource, types::ProcessorMessage},
        reconciliation::{
            capture_custody_snapshot, reconcile_against_snapshot, run_startup_reconciliation,
        },
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
        backfill::{BackfillService, StartupRange},
        checkpoint::{
            get_last_checkpoint, live_resume_slot, start_floor, wait_for_checkpoint_commit,
            BACKFILL_START_SETTING, CHECKPOINT_COMMIT_TIMEOUT_SECS, RPC_POLLING_START_SETTING,
        },
    },
    operator::escrow_sweep::CustodySnapshot,
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

/// Whether another attempt could plausibly clear this failure.
///
/// Two can. A custody-versus-ledger mismatch may be explained by rows the next fill pulls
/// in. A custody reading from behind our own ledger is a node that has not caught up, and
/// a fresh read usually lands ahead of it. Everything else, a supply breach included,
/// compares the same numbers however many times it runs, so it stops the boot on the spot.
#[cfg(feature = "datasource-rpc")]
fn reconcile_error_may_clear(error: &IndexerError) -> bool {
    matches!(
        error,
        IndexerError::Reconciliation(
            ReconciliationError::MismatchExceedsThreshold { .. }
                | ReconciliationError::CustodyBehindLedger { .. }
                | ReconciliationError::CustodySlotUnsettled { .. }
        )
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

/// Compare custody captured before the fill against the ledger it has since caught up to.
#[cfg(feature = "datasource-rpc")]
async fn reconcile_escrow_against(
    config: &ReconciliationConfig,
    common_config: &PrivateChannelIndexerConfig,
    storage: &Arc<Storage>,
    snapshot: &CustodySnapshot,
) -> Result<(), IndexerError> {
    let Some(instance_id) = common_config.escrow_instance_id else {
        return Ok(());
    };

    reconcile_against_snapshot(
        config,
        common_config.program_type,
        storage,
        &common_config.rpc_url,
        common_config.source_rpc_url.as_deref(),
        &instance_id,
        snapshot,
    )
    .await
}

/// Work out the startup range, refusing to start rather than running past an unfilled gap.
#[cfg(feature = "datasource-rpc")]
async fn resolve_startup_range(
    backfill_service: &BackfillService,
) -> Result<StartupRange, IndexerError> {
    backfill_service.resolve_range().await.inspect_err(|e| {
        error!(
            "Backfill range resolution failed; refusing to start rather than running ungated \
             past the unfilled gap: {}",
            e
        );
    })
}

/// Arm the checkpoint gate to the range about to be filled.
///
/// Sent in band on the instruction channel, so the gate is always applied before any slot
/// the fill emits and can never be leapfrogged by one that arrives first.
#[cfg(feature = "datasource-rpc")]
async fn arm_backfill_gate(
    instruction_tx: &mpsc::Sender<ProcessorMessage>,
    program_type: ProgramType,
    from_slot: u64,
    target: u64,
) -> Result<(), IndexerError> {
    send_guaranteed(
        instruction_tx,
        ProcessorMessage::Regate {
            program_type,
            from: from_slot,
            target,
        },
        "Regate (startup backfill)",
    )
    .await
    .map_err(|e| IndexerError::Backfill(BackfillError::ChannelSend(e)))
}

/// Arm the gate and fill the range, returning once every slot has been sent.
#[cfg(feature = "datasource-rpc")]
async fn arm_and_fill(
    backfill_service: &BackfillService,
    instruction_tx: &mpsc::Sender<ProcessorMessage>,
    program_type: ProgramType,
    from_slot: u64,
    target: u64,
) -> Result<(), IndexerError> {
    arm_backfill_gate(instruction_tx, program_type, from_slot, target).await?;
    backfill_service
        .run_range(from_slot, target, instruction_tx.clone())
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

    // 4d. Fill the missing range. An escrow indexer waits for that fill to become durable
    // and reconciles before the stream starts; every other program type keeps the fill
    // alongside the stream, as it was before, since it has no custody to compare against
    // and nothing to gain from booting later.
    //
    // On the escrow path custody is captured BEFORE the fill and the ledger is compared
    // against it bounded to the slot it was read at. Reading custody afterwards would
    // judge a ledger frozen at the fill target against a chain that had moved on, and any
    // deposit finalizing in between would fail the boot at the default zero tolerance.
    //
    // Waiting for the checkpoint, not merely for the fill to return, is what makes
    // "caught up" verifiable: the gate only lets the checkpoint reach the target once
    // every slot below it has been written.
    //
    // The loop is the safety net for the two things a single snapshot cannot pin down:
    // the sweep's two per-program calls can land on different slots, and a node can answer
    // from behind our own checkpoint. Both clear on a re-read. A mismatch that backfilling
    // cannot explain still fails the boot on the final attempt.
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

            // Settle the configured floor before any network call. It reads one config
            // field and one row, so surfacing it first keeps a misconfiguration from
            // being reported as whatever RPC failure happened to be hit on the way.
            start_floor(
                BACKFILL_START_SETTING,
                common_config.program_type,
                get_last_checkpoint(&storage, common_config.program_type).await?,
                indexer_config.backfill.start_slot,
            )?;

            // Only an escrow indexer has custody to compare against, so only it has a
            // reason to hold the stream back until the fill is durable.
            match common_config.escrow_instance_id {
                Some(instance_id) => {
                    for attempt in 1..=RECONCILE_MAX_ATTEMPTS {
                        // A sweep that never settled on one slot gets the same second
                        // chance a mismatch does: the node was moving under it, and the
                        // next sweep may catch it still. Anything else is fatal here.
                        let snapshot =
                            match capture_custody_snapshot(&common_config.rpc_url, &instance_id)
                                .await
                            {
                                Ok(snapshot) => snapshot,
                                Err(e)
                                    if attempt < RECONCILE_MAX_ATTEMPTS
                                        && reconcile_error_may_clear(&e) =>
                                {
                                    warn!(
                                        "Startup reconciliation attempt {}/{} could not take a \
                                     custody snapshot, re-reading: {}",
                                        attempt, RECONCILE_MAX_ATTEMPTS, e
                                    );
                                    tokio::time::sleep(Duration::from_millis(
                                        RECONCILE_RETRY_DELAY_MS,
                                    ))
                                    .await;
                                    continue;
                                }
                                Err(e) => return Err(e),
                            };

                        // The ledger must not already sit above the reading it is about to
                        // be judged by. If it does, the node is answering from behind us
                        // and there is no slot at which the two can be compared, so refuse
                        // rather than reach a verdict from an incomplete custody view.
                        let committed = get_last_checkpoint(&storage, common_config.program_type)
                            .await?
                            .unwrap_or(0);
                        if snapshot.slot < committed {
                            let behind = IndexerError::Reconciliation(
                                ReconciliationError::CustodyBehindLedger {
                                    snapshot_slot: snapshot.slot,
                                    committed,
                                },
                            );
                            if attempt < RECONCILE_MAX_ATTEMPTS {
                                warn!(
                                    "Startup reconciliation attempt {}/{}: {}, re-reading",
                                    attempt, RECONCILE_MAX_ATTEMPTS, behind
                                );
                                tokio::time::sleep(Duration::from_millis(RECONCILE_RETRY_DELAY_MS))
                                    .await;
                                continue;
                            }
                            return Err(behind);
                        }

                        let range = resolve_startup_range(&backfill_service).await?;

                        // The floor, never the target: the range above it is not filled yet.
                        #[cfg(feature = "datasource-yellowstone")]
                        {
                            startup_anchor_hint = Some(range.anchor);
                        }

                        // Fill up to the snapshot's slot rather than the chain tip. Stopping
                        // exactly where custody was measured is what makes the comparison
                        // total: the ledger ends at that slot, custody describes that slot,
                        // and there is no band of rows above it that the comparison would
                        // have to leave unexamined.
                        let ledger_end = if snapshot.slot > range.anchor {
                            arm_and_fill(
                                &backfill_service,
                                &instruction_tx,
                                common_config.program_type,
                                range.anchor,
                                snapshot.slot,
                            )
                            .await?;

                            wait_for_checkpoint_commit(
                                &storage,
                                common_config.program_type,
                                snapshot.slot,
                                Duration::from_secs(CHECKPOINT_COMMIT_TIMEOUT_SECS),
                            )
                            .await?;
                            info!("Backfill completed successfully");
                            snapshot.slot
                        } else {
                            info!("No backfill gap; checkpoint writer left ungated");
                            range.anchor
                        };

                        // One past whatever the ledger now covers. The fill stopped short of
                        // the tip, so the resolved live start would leave everything between
                        // the two unread by either producer.
                        rpc_live_start_slot = Some(ledger_end + 1);

                        match reconcile_escrow_against(
                            &indexer_config.reconciliation,
                            &common_config,
                            &storage,
                            &snapshot,
                        )
                        .await
                        {
                            Ok(()) => break,
                            Err(e)
                                if attempt < RECONCILE_MAX_ATTEMPTS
                                    && reconcile_error_may_clear(&e) =>
                            {
                                warn!(
                                    "Startup reconciliation attempt {}/{} did not balance, \
                                     retrying with a freshly read custody snapshot: {}",
                                    attempt, RECONCILE_MAX_ATTEMPTS, e
                                );
                                // Worth repeating only once the node has had a moment to
                                // settle; an immediate re-read would compare the same two
                                // numbers and burn the attempt for nothing.
                                tokio::time::sleep(Duration::from_millis(RECONCILE_RETRY_DELAY_MS))
                                    .await;
                            }
                            Err(e) => return Err(e),
                        }
                    }

                    // Only this path leaves a window between the fill and the first
                    // streamed slot; a concurrent fill still covers the cold start itself.
                    #[cfg(feature = "datasource-yellowstone")]
                    {
                        backfill_preceded_stream = true;
                    }
                }
                None => {
                    let range = resolve_startup_range(&backfill_service).await?;
                    rpc_live_start_slot = Some(range.live_start_slot);
                    #[cfg(feature = "datasource-yellowstone")]
                    {
                        startup_anchor_hint = Some(range.anchor);
                    }

                    if let Some((from_slot, target)) = range.gap {
                        // Armed out here rather than inside the task so the gate is in
                        // place before the datasource below can emit its first slot.
                        arm_backfill_gate(
                            &instruction_tx,
                            common_config.program_type,
                            from_slot,
                            target,
                        )
                        .await?;

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

            // A fill is what recovers slots below wherever the stream starts. Without one
            // the stream is the only producer, so it has to resume from the checkpoint
            // rather than the tip it would otherwise default to.
            let live_start_slot = match rpc_live_start_slot {
                Some(resolved) => Some(resolved),
                None => {
                    let checkpoint =
                        get_last_checkpoint(&storage, common_config.program_type).await?;
                    start_floor(
                        RPC_POLLING_START_SETTING,
                        common_config.program_type,
                        checkpoint,
                        rpc_config.from_slot,
                    )?;
                    live_resume_slot(checkpoint, rpc_config.from_slot)
                }
            };

            let mut source = RpcPollingSource::new(
                common_config.rpc_url.clone(),
                live_start_slot,
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
            // A lagging node usually catches up, so this one is worth re-reading.
            (
                ReconciliationError::CustodyBehindLedger {
                    snapshot_slot: 90,
                    committed: 100,
                },
                true,
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
