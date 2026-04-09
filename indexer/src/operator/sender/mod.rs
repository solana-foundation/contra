mod mint;
mod proof;
mod remint;
mod state;
mod transaction;
pub mod types;

pub use mint::{find_existing_mint_signature, find_existing_mint_signature_with_memo};
pub use types::TransactionStatusUpdate;

use crate::error::OperatorError;
use crate::operator::utils::instruction_util::TransactionBuilder;
use crate::operator::ReleaseFundsBuilderWithNonce;
use crate::operator::RpcClientWithRetry;
use crate::storage::common::storage::Storage;
use crate::ContraIndexerConfig;
use crate::ProgramType;
use solana_sdk::commitment_config::CommitmentLevel;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};
use tracing::{debug, error, info, warn};

use proof::take_pending_rotation_if_ready;
use transaction::{
    handle_transaction_submission, poll_in_flight, route_poll_results, run_poll_task,
};
use types::{PollTaskResult, SenderState, MAX_IN_FLIGHT};

/// Sends transactions to the blockchain and updates their status
///
/// Receives TransactionBuilder (either ReleaseFunds or Mint) from processor,
/// completes with SMT proofs if needed, submits to blockchain, and handles failures
#[allow(clippy::too_many_arguments)]
pub async fn run_sender(
    config: &ContraIndexerConfig,
    operator_commitment: CommitmentLevel,
    mut processor_rx: mpsc::Receiver<TransactionBuilder>,
    storage_tx: mpsc::Sender<TransactionStatusUpdate>,
    cancellation_token: tokio_util::sync::CancellationToken,
    storage: Arc<Storage>,
    retry_max_attempts: u32,
    confirmation_poll_interval_ms: u64,
    source_rpc_client: Option<Arc<RpcClientWithRetry>>,
) -> Result<(), OperatorError> {
    info!("Starting sender");

    let instance_pda = match config.program_type {
        ProgramType::Withdraw => config.escrow_instance_id,
        ProgramType::Escrow => None,
    };

    let mut state = SenderState::new(
        config,
        operator_commitment,
        instance_pda,
        storage,
        retry_max_attempts,
        confirmation_poll_interval_ms,
        source_rpc_client,
    )?;

    // Re-hydrate the deferred remint queue from any PendingRemint rows written
    // before a crash. These will be picked up by process_pending_remints on the
    // next tick
    state.recover_pending_remints(&storage_tx).await?;

    // Periodic check for pending rotation (every 500ms)
    let mut rotation_check_interval = interval(Duration::from_millis(500));

    // Channel for the poll task to deliver batched confirmation results back to the sender loop.
    let (poll_result_tx, mut poll_result_rx) = mpsc::channel(32);

    // Separate shutdown token for the poll task
    let poll_shutdown = tokio_util::sync::CancellationToken::new();

    // Spawn the dedicated poll task.
    //
    // The task handles confirmed-success entirely in-task (fires storage update +
    // metric) and pushes unconfirmed entries straight back to `in_flight`.  Only
    // on-chain errors and confirmation timeouts — rare events — come back via
    // `poll_result_rx` as `PollTaskResult::NeedsRouting`.
    // The task blocks on `in_flight.notify` when the queue is empty — zero CPU idle.
    let poll_task_handle = tokio::spawn(run_poll_task(
        state.in_flight.clone(),
        poll_result_tx,
        state.rpc_client.clone(),
        storage_tx.clone(),
        config.program_type,
        state.confirmation_poll_interval_ms,
        poll_shutdown.clone(),
    ));

    loop {
        tokio::select! {
            _ = cancellation_token.cancelled() => {
                info!("Sender received cancellation signal, draining pipeline...");
                // Drain processor channel so all pending txs are submitted.
                let mut drained_count = 0;
                while let Some(tx_builder) = processor_rx.recv().await {
                    handle_transaction_submission(&mut state, tx_builder, &storage_tx).await;
                    drained_count += 1;
                }
                info!("Sender drained {} new transactions from channel", drained_count);
                // Wait for any fire-and-forget txs to confirm before exiting.
                // The poll task has already been cancelled; drain_in_flight calls
                // poll_in_flight directly (single-cycle, no dedicated task needed).
                drain_in_flight(&mut state, &storage_tx).await;
                break;
            }

            // Receive results from the dedicated poll task.
            //
            // In the common case this arm carries only `ConfirmedSuccess` items
            // (O(1) mint_builders cleanup each).  `NeedsRouting` items — on-chain
            // errors and confirmation timeouts — are rare and go through the full
            // route_poll_results path.
            Some(results) = poll_result_rx.recv() => {
                let mut to_route = Vec::new();
                let mut confirmed_count = 0usize;
                for result in results {
                    match result {
                        PollTaskResult::ConfirmedSuccess(txn_id) => {
                            confirmed_count += 1;
                            if let Some(id) = txn_id {
                                state.mint_builders.remove(&id);
                            }
                        }
                        PollTaskResult::NeedsRouting(tx, status) => {
                            to_route.push((tx, status));
                        }
                    }
                }
                debug!(
                    confirmed = confirmed_count,
                    needs_routing = to_route.len(),
                    in_flight = state.in_flight.len(),
                    "Poll results received from poll task"
                );
                if !to_route.is_empty() {
                    route_poll_results(&mut state, to_route, &storage_tx).await;
                }
            }

            _ = rotation_check_interval.tick() => {
                // Check if pending rotation can now be executed
                if let Some(rotation_builder) = take_pending_rotation_if_ready(&mut state) {
                    info!("Executing queued ResetSmtRoot transaction");
                    let tx_builder = TransactionBuilder::ResetSmtRoot(rotation_builder);
                    handle_transaction_submission(&mut state, tx_builder, &storage_tx).await;
                }

                // Process matured deferred remints
                remint::process_pending_remints(&mut state, &storage_tx).await;

                // Process any transactions that were blocked by rotation
                while let Some((ctx, builder)) = state.rotation_retry_queue.pop() {
                    let nonce = ctx.withdrawal_nonce.expect("rotation retry must have nonce");
                    let transaction_id = ctx.transaction_id.expect("rotation retry must have transaction_id");
                    let trace_id = ctx.trace_id.clone().expect("rotation retry must have trace_id");
                    let remint_info = state.remint_cache.get(&nonce).cloned();
                    if remint_info.is_none() {
                        error!("Missing remint_info for rotation retry nonce {} - remint will not be possible on failure", nonce);
                    }
                    info!(trace_id = %trace_id, "Retrying blocked nonce {} after rotation", nonce);
                    let tx_builder = TransactionBuilder::ReleaseFunds(Box::new(
                        ReleaseFundsBuilderWithNonce { builder, nonce, transaction_id, trace_id, remint_info },
                    ));
                    handle_transaction_submission(&mut state, tx_builder, &storage_tx).await;
                }
            }

            // Back-pressure: stop consuming new transactions when in_flight is full.
            // The channel fills up → processor blocks → fetcher stops polling the DB.
            // Resumes automatically once the poll task confirms entries and drains the queue.
            tx_builder = processor_rx.recv(), if state.in_flight.len() < MAX_IN_FLIGHT => {
                if let Some(tx_builder) = tx_builder {
                    let in_flight_len = state.in_flight.len();
                    debug!(
                        in_flight = in_flight_len,
                        processor_channel_capacity = processor_rx.len(),
                        "Sender received transaction from processor"
                    );
                    handle_transaction_submission(&mut state, tx_builder, &storage_tx).await;
                } else {
                    info!("Sender channel closed");
                    // Wait for any fire-and-forget txs to confirm before exiting.
                    drain_in_flight(&mut state, &storage_tx).await;
                    break;
                }
            }
        }
    }

    // Shut down the poll task regardless of which exit path fired.
    poll_shutdown.cancel();
    drop(poll_result_rx);
    let _ = poll_task_handle.await;

    info!("Sender stopped gracefully");
    Ok(())
}

/// Wait for all in-flight fire-and-forget transactions to reach a terminal state.
///
/// Polls at state.confirmation_poll_interval_ms intervals with a 30-second wall-clock timeout.  Called on both
/// graceful shutdown paths (cancellation and channel close) so no confirmed Mint
/// transactions are orphaned at process exit.
///
/// If the timeout expires with entries still in-flight, a warning is logged and
/// the operator exits anyway — on restart the processor will re-emit any transactions
/// that lack a terminal DB status, and the idempotency memo check will prevent
/// duplicate mints if the original tx did land.
async fn drain_in_flight(
    state: &mut SenderState,
    storage_tx: &mpsc::Sender<TransactionStatusUpdate>,
) {
    if state.in_flight.is_empty() {
        return;
    }

    info!(
        count = state.in_flight.len(),
        "Draining in-flight transactions before shutdown"
    );

    let timeout_at = tokio::time::Instant::now() + Duration::from_secs(30);

    while !state.in_flight.is_empty() {
        if tokio::time::Instant::now() >= timeout_at {
            warn!(
                count = state.in_flight.len(),
                "Shutdown drain timeout — {} in-flight transactions unresolved; \
                 they will be re-processed on restart",
                state.in_flight.len(),
            );
            return;
        }

        poll_in_flight(state, storage_tx).await;

        if !state.in_flight.is_empty() {
            tokio::time::sleep(Duration::from_millis(state.confirmation_poll_interval_ms)).await;
        }
    }

    info!("All in-flight transactions resolved");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DEFAULT_CONFIRMATION_POLL_INTERVAL_MS;
    use crate::config::{PostgresConfig, ProgramType, StorageType};
    use crate::operator::sender::types::{
        InFlightQueue, InFlightTx, InstructionWithSigners, SenderState, TransactionContext,
    };
    use crate::operator::utils::instruction_util::{ExtraErrorCheckPolicy, RetryPolicy};
    use crate::operator::utils::rpc_util::{RetryConfig, RpcClientWithRetry};
    use crate::operator::MintCache;
    use crate::storage::common::storage::mock::MockStorage;
    use crate::ContraIndexerConfig;
    use solana_keychain::Signer;
    use solana_sdk::commitment_config::{CommitmentConfig, CommitmentLevel};
    use solana_sdk::pubkey::Pubkey;
    use solana_sdk::signature::Signature;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    fn make_sender_state(rpc_url: &str) -> SenderState {
        let storage = Arc::new(Storage::Mock(MockStorage::new()));
        let rpc_client = Arc::new(RpcClientWithRetry::with_retry_config(
            rpc_url.to_string(),
            RetryConfig {
                max_attempts: 1,
                base_delay: std::time::Duration::from_millis(1),
                max_delay: std::time::Duration::from_millis(1),
            },
            CommitmentConfig::confirmed(),
        ));
        SenderState {
            rpc_client: rpc_client.clone(),
            storage: storage.clone(),
            instance_pda: None,
            smt_state: None,
            retry_counts: HashMap::new(),
            mint_builders: HashMap::new(),
            mint_cache: MintCache::new(storage),
            retry_max_attempts: 3,
            confirmation_poll_interval_ms: 1,
            rotation_retry_queue: Vec::new(),
            pending_rotation: None,
            program_type: ProgramType::Escrow,
            remint_cache: HashMap::new(),
            pending_signatures: HashMap::new(),
            pending_remints: Vec::new(),
            in_flight: InFlightQueue::new(),
        }
    }

    fn make_in_flight_tx(txn_id: i64) -> InFlightTx {
        InFlightTx {
            signature: Signature::new_unique(),
            ctx: TransactionContext {
                transaction_id: Some(txn_id),
                withdrawal_nonce: None,
                trace_id: None,
            },
            instruction: InstructionWithSigners {
                instructions: vec![],
                fee_payer: Pubkey::default(),
                signers: Vec::<&'static Signer>::new(),
                compute_unit_price: None,
                compute_budget: None,
            },
            compute_unit_price: None,
            retry_policy: RetryPolicy::None,
            extra_error_checks_policy: ExtraErrorCheckPolicy::None,
            poll_attempts: 0,
            resend_count: 0,
        }
    }

    fn minimal_config() -> ContraIndexerConfig {
        ContraIndexerConfig {
            program_type: ProgramType::Escrow,
            storage_type: StorageType::Postgres,
            rpc_url: "http://localhost:8899".to_string(),
            source_rpc_url: None,
            postgres: PostgresConfig {
                database_url: "postgresql://localhost/test".to_string(),
                max_connections: 5,
            },
            escrow_instance_id: None,
        }
    }

    /// Cancellation with an already-closed processor channel must drain zero transactions
    /// and return Ok(()), confirming the graceful-shutdown path terminates without hanging.
    #[tokio::test]
    async fn run_sender_exits_when_cancelled_with_empty_channel() {
        let config = minimal_config();
        let storage = Arc::new(Storage::Mock(MockStorage::new()));
        let (processor_tx, processor_rx) = mpsc::channel(10);
        let (storage_tx, _storage_rx) = mpsc::channel(10);
        let cancellation_token = CancellationToken::new();

        // Cancel before calling run_sender so the cancellation arm fires immediately
        cancellation_token.cancel();
        // Drop processor sender so the drain loop (while let Some) completes quickly
        drop(processor_tx);

        let result = run_sender(
            &config,
            CommitmentLevel::Confirmed,
            processor_rx,
            storage_tx,
            cancellation_token,
            storage,
            3,
            DEFAULT_CONFIRMATION_POLL_INTERVAL_MS,
            None,
        )
        .await;

        assert!(result.is_ok());
    }

    /// When the processor drops its sender before any messages are sent, run_sender must
    /// detect the closed channel in the normal recv arm and return Ok(()) without cancellation.
    #[tokio::test]
    async fn run_sender_exits_when_processor_channel_closed() {
        let config = minimal_config();
        let storage = Arc::new(Storage::Mock(MockStorage::new()));

        // Create a channel and immediately close the sender side
        let processor_rx = {
            let (tx, rx) = mpsc::channel::<TransactionBuilder>(10);
            drop(tx);
            rx
        };

        let (storage_tx, _storage_rx) = mpsc::channel(10);
        let cancellation_token = CancellationToken::new();

        let result = run_sender(
            &config,
            CommitmentLevel::Confirmed,
            processor_rx,
            storage_tx,
            cancellation_token,
            storage,
            3,
            DEFAULT_CONFIRMATION_POLL_INTERVAL_MS,
            None,
        )
        .await;

        assert!(result.is_ok());
    }

    // ── drain_in_flight ───────────────────────────────────────────────

    /// An empty in-flight queue must return immediately without any RPC calls or
    /// storage updates.
    #[tokio::test]
    async fn drain_in_flight_empty_queue_returns_immediately() {
        let mut state = make_sender_state("http://localhost:8899");
        assert!(state.in_flight.is_empty());

        let (storage_tx, mut storage_rx) = mpsc::channel(10);
        drain_in_flight(&mut state, &storage_tx).await;

        assert!(state.in_flight.is_empty());
        assert!(storage_rx.try_recv().is_err(), "no storage update expected");
    }

    /// When in-flight entries never confirm, drain_in_flight must stop after the
    /// 30-second wall-clock timeout and log a warning rather than hanging forever.
    #[tokio::test(start_paused = true)]
    async fn drain_in_flight_timeout_exits_with_unresolved_entries() {
        let mut server = mockito::Server::new_async().await;

        // Always return null — entry never confirms.
        let _m = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "getSignatureStatuses"
            })))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": { "context": {"slot": 1}, "value": [null] }
                })
                .to_string(),
            )
            .expect_at_least(1)
            .create();

        let mut state = make_sender_state(&server.url());
        state.confirmation_poll_interval_ms = 100;
        state.in_flight.push(make_in_flight_tx(1));

        let (storage_tx, _storage_rx) = mpsc::channel(10);

        let drain = tokio::spawn(async move {
            drain_in_flight(&mut state, &storage_tx).await;
            state.in_flight.len() // return remaining count to assert on
        });

        // Yield once so the spawned task starts and computes `timeout_at` based on
        // time=0 (before we advance).  After this yield drain is blocked inside
        // poll_in_flight awaiting the RPC response.
        tokio::task::yield_now().await;

        // Advance the mock clock past the 30-second timeout.  The pending 100ms
        // sleep inside drain_in_flight will also be resolved by this advance.
        tokio::time::advance(Duration::from_secs(31)).await;

        let remaining = tokio::time::timeout(Duration::from_secs(5), drain)
            .await
            .expect("drain must complete after timeout advance")
            .expect("task must not panic");

        assert_eq!(
            remaining, 1,
            "unresolved entry must still be in in_flight after timeout"
        );
    }
}
