use crate::channel_utils::send_guaranteed;
use crate::error::{OperatorError, ProgramError};
use crate::metrics;
use crate::operator::utils::instruction_util::TransactionBuilder;
use crate::operator::utils::transaction_util::{check_transaction_status, ConfirmationResult};
use crate::operator::{sign_and_send_transaction, ExtraErrorCheckPolicy, RetryPolicy};
use crate::storage::common::models::TransactionStatus;
use chrono::Utc;
use contra_escrow_program_client::errors::ContraEscrowProgramError;
use contra_metrics::MetricLabel;
use solana_keychain::SolanaSigner;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::signature::Signature;
use tokio::sync::mpsc;
use tracing::{error, info, info_span, warn, Instrument};

use super::mint::{
    cleanup_mint_builder, find_existing_mint_signature, try_jit_mint_initialization,
};
use super::proof::{cleanup_failed_transaction, rebuild_with_regenerated_proof};
use super::types::{
    InstructionWithSigners, PendingRemint, SenderState, TransactionContext, TransactionStatusUpdate,
};

use std::time::Duration;

/// Safety delay before checking finality and reminting.
/// Solana finalized ≈ 32 slots × 400ms = ~12.8s. We use 2.5× safety factor.
pub const FINALITY_SAFETY_DELAY: Duration = Duration::from_secs(32);

impl SenderState {
    /// Handle incoming transaction builder (either ReleaseFunds or Mint)
    /// For ReleaseFunds: Generate SMT proof and complete builder
    /// For Mint: Just build instruction (no proof needed)
    pub(super) async fn handle_transaction_builder(
        &mut self,
        tx_builder: TransactionBuilder,
    ) -> Result<InstructionWithSigners, OperatorError> {
        let signers = tx_builder.signers();
        let compute_unit_price = tx_builder.compute_unit_price();
        let compute_budget = tx_builder.compute_budget();

        // For now fee payer is always the first signer
        let fee_payer = match signers.first() {
            Some(s) => s.pubkey(),
            None => {
                return Err(ProgramError::InvalidBuilder {
                    reason: "No signers provided".to_string(),
                }
                .into())
            }
        };

        match tx_builder {
            TransactionBuilder::ReleaseFunds(builder_with_nonce) => {
                // Cache remint info for potential recovery on permanent failure
                if let Some(ref info) = builder_with_nonce.remint_info {
                    self.remint_cache
                        .insert(builder_with_nonce.nonce, info.clone());
                }

                // Initialize SMT state lazily if needed
                if self.smt_state.is_none() {
                    self.initialize_smt_state().await?;
                }

                self.smt_state
                    .as_mut()
                    .ok_or(ProgramError::SmtNotInitialized)?
                    .handle_release_funds_transaction(
                        builder_with_nonce,
                        fee_payer,
                        signers,
                        compute_unit_price,
                        compute_budget,
                    )
            }
            // InitializeMint transaction: creates mint account via AdminVm
            TransactionBuilder::InitializeMint(_) => Ok(InstructionWithSigners {
                instructions: tx_builder.instructions()?,
                fee_payer,
                signers,
                compute_unit_price,
                compute_budget,
            }),
            TransactionBuilder::Mint(ref builder_with_txn_id) => {
                // Cache the builder for potential JIT retry
                self.mint_builders.insert(
                    builder_with_txn_id.txn_id,
                    builder_with_txn_id.builder.clone(),
                );

                // Mint transaction: creates ATA + mints tokens
                Ok(InstructionWithSigners {
                    instructions: tx_builder.instructions()?,
                    fee_payer,
                    signers,
                    compute_unit_price,
                    compute_budget,
                })
            }
            TransactionBuilder::ResetSmtRoot(ref builder) => {
                let in_flight_count = self
                    .smt_state
                    .as_ref()
                    .map(|s| s.nonce_to_builder.len())
                    .unwrap_or(0);

                if in_flight_count > 0 {
                    info!(
                        "Rotation transaction received but {} in-flight txs exist - queuing",
                        in_flight_count
                    );

                    self.pending_rotation = Some(builder.clone());

                    return Err(ProgramError::RotationPending { in_flight_count }.into());
                }

                // No in-flight transactions - process immediately
                Ok(InstructionWithSigners {
                    instructions: tx_builder.instructions()?,
                    fee_payer,
                    signers,
                    compute_budget,
                    compute_unit_price,
                })
            }
        }
    }
}

/// Top-level handler for a single transaction submission
pub async fn handle_transaction_submission(
    state: &mut SenderState,
    tx_builder: TransactionBuilder,
    storage_tx: &mpsc::Sender<TransactionStatusUpdate>,
) {
    let ctx = TransactionContext {
        transaction_id: tx_builder.transaction_id(),
        withdrawal_nonce: tx_builder.withdrawal_nonce(),
        trace_id: tx_builder.trace_id(),
    };
    let retry_policy = tx_builder.retry_policy();
    let compute_unit_price = tx_builder.compute_unit_price();
    let extra_error_checks_policy = &tx_builder.extra_error_checks_policy();

    let span = info_span!(
        "tx",
        trace_id = ctx.trace_id.as_deref().unwrap_or("none"),
        nonce = ctx.withdrawal_nonce.map(|n| n as i64),
    );

    async {
        if let TransactionBuilder::Mint(builder_with_txn_id) = &tx_builder {
            match find_existing_mint_signature(&state.rpc_client, builder_with_txn_id).await {
                Ok(Some(existing_signature)) => {
                    handle_success(state, &ctx, existing_signature, storage_tx).await;
                    return;
                }
                Ok(None) => {}
                Err(e) => {
                    error!(
                        "Mint idempotency lookup failed for transaction_id {}: {}",
                        builder_with_txn_id.txn_id, e
                    );
                    send_fatal_error(storage_tx, &ctx, &e).await;
                    return;
                }
            }
        }

        match state.handle_transaction_builder(tx_builder.clone()).await {
            Ok(instruction) => {
                info!("Transaction instruction ready for submission");
                send_and_confirm(
                    state,
                    instruction,
                    compute_unit_price,
                    &ctx,
                    retry_policy,
                    extra_error_checks_policy,
                    storage_tx,
                )
                .await;
            }
            Err(OperatorError::Program(ProgramError::RotationPending { in_flight_count })) => {
                info!(
                    "Rotation pending, waiting for {} in-flight txs to settle",
                    in_flight_count
                );
            }
            Err(OperatorError::Program(ProgramError::TreeIndexMismatch {
                nonce,
                expected_tree_index,
                current_tree_index,
            })) => {
                if let TransactionBuilder::ReleaseFunds(builder_with_nonce) = tx_builder {
                    info!(
                        "Tree index mismatch: nonce {} expects {} but current is {} - queuing for retry",
                        nonce, expected_tree_index, current_tree_index
                    );
                    state.rotation_retry_queue.push((
                        TransactionContext {
                            transaction_id: Some(builder_with_nonce.transaction_id),
                            withdrawal_nonce: Some(builder_with_nonce.nonce),
                            trace_id: Some(builder_with_nonce.trace_id),
                        },
                        builder_with_nonce.builder,
                    ));
                } else {
                    error!("TreeIndexMismatch for non-ReleaseFunds transaction");
                }
            }
            Err(e) => {
                metrics::OPERATOR_TRANSACTION_ERRORS
                    .with_label_values(&[state.program_type.as_label(), "build_error"])
                    .inc();
                error!("Failed to build transaction: {}", e);
                send_fatal_error(storage_tx, &ctx, &e.to_string()).await;
            }
        }
    }
    .instrument(span)
    .await;
}

/// Sign, send, confirm, and handle the result
pub(super) async fn send_and_confirm(
    state: &mut SenderState,
    instruction: InstructionWithSigners,
    compute_unit_price: Option<u64>,
    ctx: &TransactionContext,
    retry_policy: RetryPolicy,
    extra_error_checks_policy: &ExtraErrorCheckPolicy,
    storage_tx: &mpsc::Sender<TransactionStatusUpdate>,
) {
    // Check retry limit - only for idempotent operations that can be retried at sender level
    if let Some(nonce) = ctx.withdrawal_nonce {
        match retry_policy {
            RetryPolicy::Idempotent => {
                let attempts = state.retry_counts.get(&nonce).copied().unwrap_or(0);
                if attempts >= state.retry_max_attempts {
                    metrics::OPERATOR_TRANSACTION_ERRORS
                        .with_label_values(&[state.program_type.as_label(), "max_retries_exceeded"])
                        .inc();
                    error!(
                        "Max retries ({}) exceeded for withdrawal_nonce {}",
                        state.retry_max_attempts, nonce
                    );
                    handle_permanent_failure(state, ctx, storage_tx, "Max retries exceeded").await;
                    return;
                }
                state.retry_counts.insert(nonce, attempts + 1);
                info!(
                    "Transaction attempt {}/{} for withdrawal_nonce {}",
                    attempts + 1,
                    state.retry_max_attempts,
                    nonce
                );
            }
            RetryPolicy::None => {
                info!("Sending non-idempotent transaction - single sender-level attempt");
            }
        }
    }

    let pt = state.program_type.as_label();
    let send_start = std::time::Instant::now();

    match sign_and_send_transaction(state.rpc_client.clone(), instruction.clone(), retry_policy)
        .await
    {
        Ok(signature) => {
            info!("Transaction sent with signature: {}", signature);

            // Stash signature for finality check on withdrawal failure path
            if let Some(nonce) = ctx.withdrawal_nonce {
                state
                    .pending_signatures
                    .entry(nonce)
                    .or_default()
                    .push(signature);
            }

            let commitment_config = CommitmentConfig::confirmed();

            let result = check_transaction_status(
                state.rpc_client.clone(),
                &signature,
                commitment_config,
                extra_error_checks_policy,
            )
            .await;

            let result_label = match &result {
                Ok(ConfirmationResult::Confirmed) => "success",
                _ => "failure",
            };
            metrics::OPERATOR_RPC_SEND_DURATION
                .with_label_values(&[pt, result_label])
                .observe(send_start.elapsed().as_secs_f64());

            handle_confirmation_result(
                state,
                result,
                signature,
                compute_unit_price,
                ctx,
                instruction,
                retry_policy,
                extra_error_checks_policy,
                storage_tx,
            )
            .await;
        }
        Err(e) => {
            metrics::OPERATOR_RPC_SEND_DURATION
                .with_label_values(&[pt, "error"])
                .observe(send_start.elapsed().as_secs_f64());
            metrics::OPERATOR_TRANSACTION_ERRORS
                .with_label_values(&[pt, "rpc_send_error"])
                .inc();
            error!("Failed to send transaction: {}", e);
            handle_permanent_failure(state, ctx, storage_tx, &e.to_string()).await;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_confirmation_result<'a>(
    state: &'a mut SenderState,
    result: Result<ConfirmationResult, crate::error::TransactionError>,
    signature: Signature,
    compute_unit_price: Option<u64>,
    ctx: &'a TransactionContext,
    instruction: InstructionWithSigners,
    retry_policy: RetryPolicy,
    extra_error_checks_policy: &'a ExtraErrorCheckPolicy,
    storage_tx: &'a mpsc::Sender<TransactionStatusUpdate>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        let pt = state.program_type.as_label();
        match result {
            Ok(ConfirmationResult::Confirmed) => {
                handle_success(state, ctx, signature, storage_tx).await;
            }
            Ok(ConfirmationResult::Failed(Some(ContraEscrowProgramError::InvalidSmtProof))) => {
                metrics::OPERATOR_TRANSACTION_ERRORS
                    .with_label_values(&[pt, "invalid_smt_proof"])
                    .inc();
                warn!("InvalidSmtProof - removing nonce and rebuilding with fresh proof");
                if let (Some(nonce), Some(ref mut smt_state)) =
                    (ctx.withdrawal_nonce, state.smt_state.as_mut())
                {
                    smt_state.smt_state.remove_nonce(nonce);
                }
                if let Some(new_instruction) =
                    rebuild_with_regenerated_proof(state, ctx.withdrawal_nonce, instruction).await
                {
                    send_and_confirm(
                        state,
                        new_instruction,
                        compute_unit_price,
                        ctx,
                        retry_policy,
                        extra_error_checks_policy,
                        storage_tx,
                    )
                    .await;
                } else {
                    handle_permanent_failure(state, ctx, storage_tx, "Failed to rebuild proof")
                        .await;
                }
            }
            Ok(ConfirmationResult::Failed(Some(
                ContraEscrowProgramError::InvalidTransactionNonceForCurrentTreeIndex,
            ))) => {
                metrics::OPERATOR_TRANSACTION_ERRORS
                    .with_label_values(&[pt, "invalid_nonce_for_tree_index"])
                    .inc();
                error!("InvalidTransactionNonce - fatal error");
                handle_permanent_failure(state, ctx, storage_tx, "Invalid nonce for tree index")
                    .await;
            }
            Ok(ConfirmationResult::MintNotInitialized) => {
                metrics::OPERATOR_TRANSACTION_ERRORS
                    .with_label_values(&[pt, "mint_not_initialized"])
                    .inc();
                if let Some(txn_id) = ctx.transaction_id {
                    if state.mint_builders.contains_key(&txn_id) {
                        warn!("Mint account not initialized - attempting JIT initialization");

                        if let Some(new_instruction) =
                            try_jit_mint_initialization(state, txn_id, instruction.clone()).await
                        {
                            info!("Retrying with JIT mint initialization");
                            send_and_confirm(
                                state,
                                new_instruction,
                                compute_unit_price,
                                ctx,
                                retry_policy,
                                extra_error_checks_policy,
                                storage_tx,
                            )
                            .await;
                        } else {
                            handle_permanent_failure(
                                state,
                                ctx,
                                storage_tx,
                                "Mint initialization failed",
                            )
                            .await;
                        }
                    } else {
                        error!("MintNotInitialized error for non-Mint transaction");
                        handle_permanent_failure(state, ctx, storage_tx, "Unexpected mint error")
                            .await;
                    }
                } else {
                    error!("MintNotInitialized error without transaction_id");
                    handle_permanent_failure(state, ctx, storage_tx, "Mint initialization failed")
                        .await;
                }
            }
            Ok(ConfirmationResult::Retry) => match retry_policy {
                RetryPolicy::None => {
                    metrics::OPERATOR_TRANSACTION_ERRORS
                        .with_label_values(&[pt, "confirmation_timeout_non_idempotent"])
                        .inc();
                    error!("Confirmation failed for non-idempotent operation - status unknown, cannot retry");
                    handle_permanent_failure(
                        state,
                        ctx,
                        storage_tx,
                        "Confirmation failed - transaction status unknown, unsafe to retry",
                    )
                    .await;
                }
                RetryPolicy::Idempotent => {
                    metrics::OPERATOR_TRANSACTION_ERRORS
                        .with_label_values(&[pt, "confirmation_timeout"])
                        .inc();
                    warn!("Confirmation failed for idempotent operation - retrying (nonce protects against duplicates)");
                    send_and_confirm(
                        state,
                        instruction,
                        compute_unit_price,
                        ctx,
                        retry_policy,
                        extra_error_checks_policy,
                        storage_tx,
                    )
                    .await;
                }
            },
            Ok(ConfirmationResult::Failed(program_error)) => {
                metrics::OPERATOR_TRANSACTION_ERRORS
                    .with_label_values(&[pt, "program_error"])
                    .inc();
                error!("Other program error: {:?}", program_error);
                handle_permanent_failure(state, ctx, storage_tx, &format!("{:?}", program_error))
                    .await;
            }
            Err(e) => {
                metrics::OPERATOR_TRANSACTION_ERRORS
                    .with_label_values(&[pt, "confirmation_error"])
                    .inc();
                error!("Confirmation error: {}", e);
                handle_permanent_failure(state, ctx, storage_tx, &e.to_string()).await;
            }
        }
    })
}

/// Handle successful transaction confirmation
pub(super) async fn handle_success(
    state: &mut SenderState,
    ctx: &TransactionContext,
    signature: Signature,
    storage_tx: &mpsc::Sender<TransactionStatusUpdate>,
) {
    info!("Transaction confirmed: {}", signature);

    // Handle ReleaseFunds (withdrawal nonce-based) transactions
    if let (Some(nonce), Some(ref mut smt_state)) = (ctx.withdrawal_nonce, state.smt_state.as_mut())
    {
        smt_state.nonce_to_builder.remove(&nonce);
        state.retry_counts.remove(&nonce);
        state.remint_cache.remove(&nonce);
        state.pending_signatures.remove(&nonce);
        info!("Cleaned up state for withdrawal_nonce {}", nonce);

        if let Some(txn_id) = ctx.transaction_id {
            send_guaranteed(
                storage_tx,
                TransactionStatusUpdate {
                    transaction_id: txn_id,
                    trace_id: ctx.trace_id.clone(),
                    status: TransactionStatus::Completed,
                    counterpart_signature: Some(signature.to_string()),
                    processed_at: Some(Utc::now()),
                    error_message: None,
                    remint_signature: None,
                    remint_attempted: false,
                },
                "transaction status update",
            )
            .await
            .ok();
        }
    }
    // Handle Mint (transaction_id-based) transactions
    else if let Some(transaction_id) = ctx.transaction_id {
        info!("Updating database for transaction_id {}", transaction_id);

        metrics::OPERATOR_MINTS_SENT
            .with_label_values(&[state.program_type.as_label()])
            .inc();

        cleanup_mint_builder(state, Some(transaction_id));

        send_guaranteed(
            storage_tx,
            TransactionStatusUpdate {
                transaction_id,
                trace_id: ctx.trace_id.clone(),
                status: TransactionStatus::Completed,
                counterpart_signature: Some(signature.to_string()),
                processed_at: Some(Utc::now()),
                error_message: None,
                remint_signature: None,
                remint_attempted: false,
            },
            "transaction status update",
        )
        .await
        .ok();
    }
    // Handle ResetSmtRoot (no transaction_id) - update local SMT tree index
    else if let Some(ref mut smt_state) = state.smt_state {
        let new_tree_index = smt_state.smt_state.tree_index() + 1;
        smt_state.smt_state.reset(new_tree_index);
        info!(
            "Tree rotation complete! Updated local SMT to tree_index {}",
            new_tree_index
        );
    }
}

/// Handle permanent transaction failure with deferred remint for withdrawals.
///
/// For withdrawal transactions: removes remint info from cache, runs cleanup
/// (which removes the nonce from SMT and builder caches), then queues a deferred
/// remint that will execute after the Solana finality window passes. This prevents
/// double-spend if the original withdrawal lands on-chain after our polling window.
///
/// For non-withdrawal transactions: delegates to send_fatal_error.
pub(super) async fn handle_permanent_failure(
    state: &mut SenderState,
    ctx: &TransactionContext,
    storage_tx: &mpsc::Sender<TransactionStatusUpdate>,
    error_msg: &str,
) {
    // Extract remint info BEFORE cleanup destroys builder cache
    let remint_info = ctx
        .withdrawal_nonce
        .and_then(|nonce| state.remint_cache.remove(&nonce));

    // Collect stashed signatures for finality check
    let signatures = ctx
        .withdrawal_nonce
        .and_then(|nonce| state.pending_signatures.remove(&nonce))
        .unwrap_or_default();

    cleanup_failed_transaction(state, ctx.withdrawal_nonce);

    let Some(info) = remint_info else {
        // Not a withdrawal — use normal fatal error path
        send_fatal_error(storage_tx, ctx, error_msg).await;
        return;
    };

    // Zero signatures means sign_and_send itself failed — we have nothing to verify.
    // The RPC may have broadcast the tx before erroring, so blind remint is unsafe.
    if signatures.is_empty() {
        error!(
            "No signatures to verify for nonce {:?} — cannot safely remint, sending to ManualReview",
            ctx.withdrawal_nonce,
        );
        if let Some(transaction_id) = ctx.transaction_id {
            send_guaranteed(
                storage_tx,
                TransactionStatusUpdate {
                    transaction_id,
                    trace_id: ctx.trace_id.clone(),
                    status: TransactionStatus::ManualReview,
                    counterpart_signature: None,
                    processed_at: Some(Utc::now()),
                    error_message: Some(format!(
                        "{} | no signatures to verify — remint unsafe",
                        error_msg
                    )),
                    remint_signature: None,
                    remint_attempted: false,
                },
                "transaction status update",
            )
            .await
            .ok();
        }
        return;
    }

    let deadline = Utc::now() + chrono::Duration::from_std(FINALITY_SAFETY_DELAY).unwrap();

    // Atomically transition to PendingRemint, persisting the withdrawal signatures
    // needed for the finality check. This replaces the previous Failed write —
    // keeping status as Processing until the remint resolves avoids partial state
    // if the operator crashes during the finality window.
    if let Some(transaction_id) = ctx.transaction_id {
        let sig_strings: Vec<String> = signatures.iter().map(|sig| sig.to_string()).collect();

        if let Err(e) = state
            .storage
            .set_pending_remint(transaction_id, sig_strings, deadline)
            .await
        {
            error!(
                "Failed to persist PendingRemint for transaction {} - sending to manual review: {}",
                transaction_id, e
            );
            send_guaranteed(
                storage_tx,
                TransactionStatusUpdate {
                    transaction_id,
                    trace_id: ctx.trace_id.clone(),
                    status: TransactionStatus::ManualReview,
                    counterpart_signature: None,
                    processed_at: Some(Utc::now()),
                    error_message: Some(format!(
                        "{} | failed to persist pending remint: {}",
                        error_msg, e
                    )),
                    remint_signature: None,
                    remint_attempted: false,
                },
                "transaction status update",
            )
            .await
            .ok();
            return;
        }
    }

    // `transaction_id` is always `Some` at this point in practice — only
    // `ReleaseFunds` transactions populate `remint_cache`, and `ReleaseFunds`
    // always carries a DB transaction_id (see `TransactionBuilder::transaction_id`
    // in instruction_util.rs). `InitializeMint` and `ResetSmtRoot` return `None`
    // there and would have exited early above via `send_fatal_error`. This guard
    // exists to prevent silently enqueuing a `PendingRemint` with no DB record,
    // which would be lost on restart since recovery reads from the DB.
    if ctx.transaction_id.is_none() {
        error!(
            "Cannot defer remint for nonce {:?} — no transaction_id, entry would be unrecoverable on restart",
            ctx.withdrawal_nonce,
        );
        return;
    }

    info!(
        "Remint deferred for finality check ({}s) — {} signature(s) to verify for nonce {:?}",
        FINALITY_SAFETY_DELAY.as_secs(),
        signatures.len(),
        ctx.withdrawal_nonce,
    );

    state.pending_remints.push(PendingRemint {
        ctx: ctx.clone(),
        remint_info: info,
        signatures,
        original_error: error_msg.to_string(),
        deadline,
        finality_check_attempts: 0,
    });
}

/// Helper for fatal errors (Failed status, no signature)
pub(super) async fn send_fatal_error(
    storage_tx: &mpsc::Sender<TransactionStatusUpdate>,
    ctx: &TransactionContext,
    error_msg: &str,
) {
    if let Some(transaction_id) = ctx.transaction_id {
        send_guaranteed(
            storage_tx,
            TransactionStatusUpdate {
                transaction_id,
                trace_id: ctx.trace_id.clone(),
                status: TransactionStatus::Failed,
                counterpart_signature: None,
                processed_at: Some(Utc::now()),
                error_message: Some(error_msg.to_string()),
                remint_signature: None,
                remint_attempted: false,
            },
            "transaction status update",
        )
        .await
        .ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProgramType;
    use crate::error::TransactionError;
    use crate::operator::sender::types::SenderSMTState;
    use crate::operator::utils::instruction_util::WithdrawalRemintInfo;
    use crate::operator::utils::rpc_util::{RetryConfig, RpcClientWithRetry};
    use crate::operator::utils::smt_util::SmtState;
    use crate::operator::MintCache;
    use crate::storage::common::storage::mock::MockStorage;
    use crate::storage::common::storage::Storage;
    use contra_escrow_program_client::errors::ContraEscrowProgramError;
    use contra_escrow_program_client::instructions::ReleaseFundsBuilder;
    use solana_keychain::Signer;
    use solana_sdk::commitment_config::CommitmentConfig;
    use solana_sdk::pubkey::Pubkey;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn dummy_instruction() -> InstructionWithSigners {
        InstructionWithSigners {
            instructions: vec![],
            fee_payer: Pubkey::default(),
            signers: Vec::<&'static Signer>::new(),
            compute_unit_price: None,
            compute_budget: None,
        }
    }

    fn make_sender_state() -> SenderState {
        let mock = MockStorage::new();
        let storage = Arc::new(Storage::Mock(mock));
        let rpc_client = Arc::new(RpcClientWithRetry::with_retry_config(
            "http://localhost:8899".to_string(),
            RetryConfig::default(),
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
            rotation_retry_queue: Vec::new(),
            pending_rotation: None,
            program_type: ProgramType::Escrow,
            remint_cache: HashMap::new(),
            pending_signatures: HashMap::new(),
            pending_remints: Vec::new(),
        }
    }

    fn make_remint_info(txn_id: i64) -> WithdrawalRemintInfo {
        WithdrawalRemintInfo {
            transaction_id: txn_id,
            trace_id: format!("trace-{txn_id}"),
            mint: solana_sdk::pubkey::Pubkey::new_unique(),
            user: solana_sdk::pubkey::Pubkey::new_unique(),
            user_ata: solana_sdk::pubkey::Pubkey::new_unique(),
            token_program: spl_token::id(),
            amount: 5000,
        }
    }

    // ── handle_permanent_failure ─────────────────────────────────────

    #[tokio::test]
    async fn permanent_failure_non_withdrawal_sends_failed_status() {
        let mut state = make_sender_state();
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        let ctx = TransactionContext {
            transaction_id: Some(42),
            withdrawal_nonce: None, // not a withdrawal
            trace_id: Some("trace-42".to_string()),
        };

        handle_permanent_failure(&mut state, &ctx, &storage_tx, "some error").await;

        let update = storage_rx.try_recv().expect("should receive status update");
        assert_eq!(update.transaction_id, 42);
        assert_eq!(update.status, TransactionStatus::Failed);
        assert_eq!(update.error_message.as_deref(), Some("some error"));
        assert!(update.remint_signature.is_none());
    }

    #[tokio::test]
    async fn permanent_failure_withdrawal_no_cache_sends_failed_status() {
        let mut state = make_sender_state();
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        // Withdrawal nonce but nothing in remint_cache
        let ctx = TransactionContext {
            transaction_id: Some(7),
            withdrawal_nonce: Some(99),
            trace_id: Some("trace-7".to_string()),
        };

        handle_permanent_failure(&mut state, &ctx, &storage_tx, "max retries").await;

        let update = storage_rx.try_recv().expect("should receive status update");
        assert_eq!(update.status, TransactionStatus::Failed);
        assert_eq!(update.error_message.as_deref(), Some("max retries"));
        assert!(update.remint_signature.is_none());
    }

    #[tokio::test]
    async fn permanent_failure_withdrawal_with_cache_defers_remint() {
        let mut state = make_sender_state();
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        // Populate remint cache and some pending signatures
        state.remint_cache.insert(5, make_remint_info(10));
        let sig = Signature::new_unique();
        state.pending_signatures.insert(5, vec![sig]);

        let ctx = TransactionContext {
            transaction_id: Some(10),
            withdrawal_nonce: Some(5),
            trace_id: Some("trace-10".to_string()),
        };

        handle_permanent_failure(&mut state, &ctx, &storage_tx, "release_funds failed").await;

        // No immediate status update — transaction remains in PendingRemint in DB
        // until process_pending_remints resolves it after the finality window.
        assert!(
            storage_rx.try_recv().is_err(),
            "should NOT send a status update while remint is deferred"
        );

        // Entry should be in pending_remints
        assert_eq!(state.pending_remints.len(), 1);
        let entry = &state.pending_remints[0];
        assert_eq!(entry.ctx.transaction_id, Some(10));
        assert_eq!(entry.signatures.len(), 1);
        assert_eq!(entry.signatures[0], sig);
        assert_eq!(entry.original_error, "release_funds failed");
        assert_eq!(entry.finality_check_attempts, 0);

        // remint_cache and pending_signatures should be drained
        assert!(!state.remint_cache.contains_key(&5));
        assert!(!state.pending_signatures.contains_key(&5));
    }

    #[tokio::test]
    async fn permanent_failure_zero_sigs_sends_manual_review() {
        let mut state = make_sender_state();
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        // Remint cache present but NO pending signatures (sign_and_send itself failed)
        state.remint_cache.insert(5, make_remint_info(10));
        // Note: not inserting into pending_signatures

        let ctx = TransactionContext {
            transaction_id: Some(10),
            withdrawal_nonce: Some(5),
            trace_id: Some("trace-10".to_string()),
        };

        handle_permanent_failure(&mut state, &ctx, &storage_tx, "rpc send error").await;

        // Should go straight to ManualReview — no deferred remint
        let update = storage_rx
            .try_recv()
            .expect("should receive ManualReview status");
        assert_eq!(update.transaction_id, 10);
        assert_eq!(update.status, TransactionStatus::ManualReview);
        let err = update.error_message.as_deref().unwrap();
        assert!(
            err.contains("no signatures to verify"),
            "should mention no sigs: {err}"
        );

        // Nothing queued
        assert!(
            state.pending_remints.is_empty(),
            "should not queue deferred remint with zero sigs"
        );
    }

    // ── handle_success ──────────────────────────────────────────────

    #[tokio::test]
    async fn success_clears_remint_cache_and_nonce_state() {
        let mut state = make_sender_state();
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        // Set up SMT state with a cached builder at nonce 3
        let mut smt = SenderSMTState {
            smt_state: SmtState::new(0),
            nonce_to_builder: HashMap::new(),
        };
        let ctx = TransactionContext {
            transaction_id: Some(50),
            withdrawal_nonce: Some(3),
            trace_id: Some("trace-50".to_string()),
        };
        smt.nonce_to_builder
            .insert(3, (ctx.clone(), ReleaseFundsBuilder::new()));
        state.smt_state = Some(smt);
        state.retry_counts.insert(3, 2);
        state.remint_cache.insert(3, make_remint_info(50));
        state
            .pending_signatures
            .insert(3, vec![Signature::new_unique()]);

        let sig = solana_sdk::signature::Signature::new_unique();
        handle_success(&mut state, &ctx, sig, &storage_tx).await;

        // All nonce-keyed state should be cleaned up
        let smt = state.smt_state.as_ref().unwrap();
        assert!(!smt.nonce_to_builder.contains_key(&3));
        assert!(!state.retry_counts.contains_key(&3));
        assert!(
            !state.remint_cache.contains_key(&3),
            "remint_cache should be cleared on success"
        );
        assert!(
            !state.pending_signatures.contains_key(&3),
            "pending_signatures should be cleared on success"
        );

        // Should send Completed status
        let update = storage_rx.try_recv().expect("should receive status update");
        assert_eq!(update.transaction_id, 50);
        assert_eq!(update.status, TransactionStatus::Completed);
    }

    #[tokio::test]
    async fn send_and_confirm_stashes_withdrawal_signature() {
        let mut state = make_sender_state();
        let nonce = 42u64;

        // Simulate what send_and_confirm does: stash a signature
        let sig = Signature::new_unique();
        state.pending_signatures.entry(nonce).or_default().push(sig);

        assert!(state.pending_signatures.contains_key(&nonce));
        assert_eq!(state.pending_signatures[&nonce].len(), 1);
        assert_eq!(state.pending_signatures[&nonce][0], sig);

        // Stash another (simulating a retry)
        let sig2 = Signature::new_unique();
        state
            .pending_signatures
            .entry(nonce)
            .or_default()
            .push(sig2);
        assert_eq!(state.pending_signatures[&nonce].len(), 2);
    }

    // ── set_pending_remint persistence ───────────────────────────────

    /// When a withdrawal fails permanently and is eligible for remint,
    /// `handle_permanent_failure` must persist the PendingRemint state to
    /// the database before queuing the entry in memory.
    ///
    /// This test verifies three things that are critical for crash safety:
    ///   1. `set_pending_remint` is called exactly once with the correct transaction_id.
    ///   2. All withdrawal signatures are stored — missing even one could cause a
    ///      false "not finalized" result on recovery, leading to a duplicate remint.
    ///   3. The deadline is ~32s in the future so recovery restores the correct wait
    ///      time rather than firing the remint immediately on restart.
    #[tokio::test]
    async fn permanent_failure_calls_set_pending_remint_with_correct_args() {
        let mut state = make_sender_state();
        let (storage_tx, _storage_rx) = mpsc::channel(10);

        // Two signatures — simulating a withdrawal that was retried once before
        // failing permanently. Both must be persisted for a complete finality check.
        let sig1 = Signature::new_unique();
        let sig2 = Signature::new_unique();
        state.remint_cache.insert(5, make_remint_info(10));
        state.pending_signatures.insert(5, vec![sig1, sig2]);

        let ctx = TransactionContext {
            transaction_id: Some(10),
            withdrawal_nonce: Some(5),
            trace_id: Some("trace-10".to_string()),
        };

        let before = Utc::now();
        handle_permanent_failure(&mut state, &ctx, &storage_tx, "release_funds failed").await;
        let after = Utc::now();

        // Extract the mock to inspect what was written to storage.
        let Storage::Mock(ref mock) = *state.storage else {
            panic!("expected mock storage");
        };
        let calls = mock.pending_remint_signatures.lock().unwrap();

        assert_eq!(
            calls.len(),
            1,
            "set_pending_remint should be called exactly once"
        );

        let (stored_id, stored_sigs, stored_deadline) = &calls[0];
        assert_eq!(*stored_id, 10, "wrong transaction_id persisted");

        assert_eq!(
            stored_sigs.len(),
            2,
            "both withdrawal signatures must be persisted"
        );
        assert!(
            stored_sigs.contains(&sig1.to_string()),
            "sig1 must be persisted"
        );
        assert!(
            stored_sigs.contains(&sig2.to_string()),
            "sig2 must be persisted"
        );

        // Deadline must be ~FINALITY_SAFETY_DELAY (32s) from now.
        // We allow a ±3s window to absorb test execution time.
        let expected_min = before + chrono::Duration::seconds(29);
        let expected_max = after + chrono::Duration::seconds(35);
        assert!(
            *stored_deadline >= expected_min && *stored_deadline <= expected_max,
            "deadline should be ~32s from now, got {stored_deadline}"
        );
    }

    /// When the database write for `set_pending_remint` fails, the operator
    /// cannot safely defer the remint — it has no guarantee the state will
    /// survive a restart. Instead of silently losing the remint, it must
    /// immediately escalate to ManualReview so an operator can intervene.
    ///
    /// Equally important: nothing should be queued in `pending_remints`.
    /// Queuing in memory without the DB write would be a half-written state —
    /// the entry would disappear on the next crash, violating the atomicity
    /// invariant.
    #[tokio::test]
    async fn permanent_failure_sends_manual_review_when_storage_fails() {
        let mut state = make_sender_state();
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        // Instruct the mock to fail on set_pending_remint.
        let Storage::Mock(ref mock) = *state.storage else {
            panic!("expected mock storage");
        };
        mock.set_should_fail("set_pending_remint", true);

        state.remint_cache.insert(5, make_remint_info(10));
        state
            .pending_signatures
            .insert(5, vec![Signature::new_unique()]);

        let ctx = TransactionContext {
            transaction_id: Some(10),
            withdrawal_nonce: Some(5),
            trace_id: Some("trace-10".to_string()),
        };

        handle_permanent_failure(&mut state, &ctx, &storage_tx, "release_funds failed").await;

        // Must escalate to ManualReview — human intervention is needed.
        let update = storage_rx
            .try_recv()
            .expect("should receive ManualReview status");
        assert_eq!(update.transaction_id, 10);
        assert_eq!(update.status, TransactionStatus::ManualReview);

        // Must not queue in memory — no DB write means no crash safety.
        assert!(
            state.pending_remints.is_empty(),
            "should not queue pending remint when storage write failed"
        );
    }

    /// `send_fatal_error` must emit a `Failed` status update with the exact error message
    /// and no counterpart signature when the context contains a transaction id.
    #[tokio::test]
    async fn send_fatal_error_with_transaction_id_sends_failed_status() {
        let (tx, mut rx) = mpsc::channel(10);
        let ctx = TransactionContext {
            transaction_id: Some(42),
            withdrawal_nonce: None,
            trace_id: Some("trace-1".to_string()),
        };

        send_fatal_error(&tx, &ctx, "test error").await;

        let update = rx.recv().await.unwrap();
        assert_eq!(update.transaction_id, 42);
        assert_eq!(update.status, TransactionStatus::Failed);
        assert!(update.counterpart_signature.is_none());
        assert_eq!(update.error_message.as_deref(), Some("test error"));
    }

    /// Without a transaction id there is nothing to mark as failed, so `send_fatal_error`
    /// must silently drop the error and send nothing to the storage channel.
    #[tokio::test]
    async fn send_fatal_error_without_transaction_id_sends_nothing() {
        let (tx, mut rx) = mpsc::channel(10);
        let ctx = TransactionContext {
            transaction_id: None,
            withdrawal_nonce: None,
            trace_id: None,
        };

        send_fatal_error(&tx, &ctx, "test error").await;

        drop(tx);
        assert!(rx.recv().await.is_none());
    }

    /// A successful mint (no withdrawal nonce) must emit `Completed` with the on-chain
    /// signature as `counterpart_signature`.
    #[tokio::test]
    async fn handle_success_mint_transaction_sends_completed_status() {
        let mut state = make_sender_state();
        let (tx, mut rx) = mpsc::channel(10);
        let ctx = TransactionContext {
            transaction_id: Some(7),
            withdrawal_nonce: None,
            trace_id: Some("trace-mint".to_string()),
        };
        let sig = Signature::new_unique();

        handle_success(&mut state, &ctx, sig, &tx).await;

        let update = rx.recv().await.unwrap();
        assert_eq!(update.transaction_id, 7);
        assert_eq!(update.status, TransactionStatus::Completed);
        assert_eq!(
            update.counterpart_signature.as_deref(),
            Some(sig.to_string().as_str())
        );
    }

    /// A confirmed ResetSmtRoot transaction (no transaction_id, no nonce) must advance the
    /// tree index and send no status update to the storage channel.
    #[tokio::test]
    async fn handle_success_reset_smt_root_increments_tree_index() {
        let mut state = make_sender_state();
        // Set up SMT state
        state.smt_state = Some(super::super::types::SenderSMTState {
            smt_state: crate::operator::utils::smt_util::SmtState::new(0),
            nonce_to_builder: HashMap::new(),
        });

        let (tx, mut rx) = mpsc::channel(10);
        // No transaction_id, no withdrawal_nonce = ResetSmtRoot context
        let ctx = TransactionContext {
            transaction_id: None,
            withdrawal_nonce: None,
            trace_id: None,
        };
        let sig = Signature::new_unique();

        handle_success(&mut state, &ctx, sig, &tx).await;

        // No status update sent for ResetSmtRoot
        drop(tx);
        assert!(rx.recv().await.is_none());

        // Tree index should be incremented
        assert_eq!(state.smt_state.as_ref().unwrap().smt_state.tree_index(), 1);
    }

    /// After a successful withdrawal, the per-nonce retry counter must be removed so that
    /// a future submission with the same nonce starts from a clean slate.
    #[tokio::test]
    async fn handle_success_withdrawal_cleans_up_nonce_state() {
        let mut state = make_sender_state();
        state.instance_pda = Some(Pubkey::new_unique());
        state.smt_state = Some(super::super::types::SenderSMTState {
            smt_state: crate::operator::utils::smt_util::SmtState::new(0),
            nonce_to_builder: HashMap::new(),
        });
        state.retry_counts.insert(5, 2);

        let (tx, mut rx) = mpsc::channel(10);
        let ctx = TransactionContext {
            transaction_id: Some(99),
            withdrawal_nonce: Some(5),
            trace_id: Some("trace-wd".to_string()),
        };
        let sig = Signature::new_unique();

        handle_success(&mut state, &ctx, sig, &tx).await;

        let update = rx.recv().await.unwrap();
        assert_eq!(update.transaction_id, 99);
        assert_eq!(update.status, TransactionStatus::Completed);

        // Retry count should be cleaned up
        assert!(!state.retry_counts.contains_key(&5));
    }

    // ============================================================
    // handle_confirmation_result tests (code paths that don't need RPC)
    // ============================================================

    /// `InvalidTransactionNonceForCurrentTreeIndex` is a permanent on-chain rejection; the
    /// transaction must be marked Failed and the error message must mention "nonce".
    #[tokio::test]
    async fn confirmation_result_invalid_nonce_for_tree_index_sends_fatal_error() {
        let mut state = make_sender_state();
        let (tx, mut rx) = mpsc::channel(10);
        let ctx = TransactionContext {
            transaction_id: Some(10),
            withdrawal_nonce: None,
            trace_id: None,
        };

        handle_confirmation_result(
            &mut state,
            Ok(ConfirmationResult::Failed(Some(
                ContraEscrowProgramError::InvalidTransactionNonceForCurrentTreeIndex,
            ))),
            Signature::new_unique(),
            None,
            &ctx,
            dummy_instruction(),
            RetryPolicy::None,
            &ExtraErrorCheckPolicy::None,
            &tx,
        )
        .await;

        let update = rx.recv().await.unwrap();
        assert_eq!(update.transaction_id, 10);
        assert_eq!(update.status, TransactionStatus::Failed);
        assert!(update
            .error_message
            .as_deref()
            .unwrap_or("")
            .contains("nonce"));
    }

    /// An unrecognised program error (None variant) is treated as a permanent failure;
    /// the transaction must be marked Failed with no retry attempt.
    #[tokio::test]
    async fn confirmation_result_other_program_error_sends_fatal_error() {
        let mut state = make_sender_state();
        let (tx, mut rx) = mpsc::channel(10);
        let ctx = TransactionContext {
            transaction_id: Some(11),
            withdrawal_nonce: None,
            trace_id: None,
        };

        handle_confirmation_result(
            &mut state,
            Ok(ConfirmationResult::Failed(None)),
            Signature::new_unique(),
            None,
            &ctx,
            dummy_instruction(),
            RetryPolicy::None,
            &ExtraErrorCheckPolicy::None,
            &tx,
        )
        .await;

        let update = rx.recv().await.unwrap();
        assert_eq!(update.transaction_id, 11);
        assert_eq!(update.status, TransactionStatus::Failed);
    }

    /// A `Retry` result with `RetryPolicy::None` (non-idempotent operation) cannot be safely
    /// retried, so it must be converted to a fatal failure with an "unknown" error message.
    #[tokio::test]
    async fn confirmation_result_retry_with_none_policy_sends_fatal_error() {
        let mut state = make_sender_state();
        let (tx, mut rx) = mpsc::channel(10);
        let ctx = TransactionContext {
            transaction_id: Some(12),
            withdrawal_nonce: None,
            trace_id: None,
        };

        handle_confirmation_result(
            &mut state,
            Ok(ConfirmationResult::Retry),
            Signature::new_unique(),
            None,
            &ctx,
            dummy_instruction(),
            RetryPolicy::None,
            &ExtraErrorCheckPolicy::None,
            &tx,
        )
        .await;

        let update = rx.recv().await.unwrap();
        assert_eq!(update.transaction_id, 12);
        assert_eq!(update.status, TransactionStatus::Failed);
        assert!(update
            .error_message
            .as_deref()
            .unwrap_or("")
            .contains("unknown"));
    }

    /// An RPC transport error bubbled up as `TransactionError::Rpc` must result in a Failed
    /// status update; the error message must contain the original RPC error text.
    #[tokio::test]
    async fn confirmation_result_rpc_error_sends_fatal_error() {
        let mut state = make_sender_state();
        let (tx, mut rx) = mpsc::channel(10);
        let ctx = TransactionContext {
            transaction_id: Some(13),
            withdrawal_nonce: None,
            trace_id: None,
        };

        let rpc_err = Box::new(
            solana_rpc_client_api::client_error::Error::new_with_request(
                solana_rpc_client_api::client_error::ErrorKind::Custom(
                    "test rpc error".to_string(),
                ),
                solana_rpc_client_api::request::RpcRequest::GetBalance,
            ),
        );

        handle_confirmation_result(
            &mut state,
            Err(TransactionError::Rpc(rpc_err)),
            Signature::new_unique(),
            None,
            &ctx,
            dummy_instruction(),
            RetryPolicy::None,
            &ExtraErrorCheckPolicy::None,
            &tx,
        )
        .await;

        let update = rx.recv().await.unwrap();
        assert_eq!(update.transaction_id, 13);
        assert_eq!(update.status, TransactionStatus::Failed);
        assert!(
            update
                .error_message
                .as_deref()
                .unwrap_or("")
                .contains("test rpc error"),
            "expected error message to contain RPC error text, got: {:?}",
            update.error_message
        );
    }

    /// When `MintNotInitialized` fires but no matching mint builder exists in state, the
    /// fallback path must emit a fatal error so the transaction is not silently dropped.
    #[tokio::test]
    async fn confirmation_result_mint_not_initialized_no_transaction_id_sends_fatal_error() {
        let mut state = make_sender_state();
        let (tx, mut rx) = mpsc::channel(10);
        let ctx = TransactionContext {
            transaction_id: Some(14),
            withdrawal_nonce: None,
            trace_id: None,
        };

        handle_confirmation_result(
            &mut state,
            Ok(ConfirmationResult::MintNotInitialized),
            Signature::new_unique(),
            None,
            &ctx,
            dummy_instruction(),
            RetryPolicy::None,
            &ExtraErrorCheckPolicy::None,
            &tx,
        )
        .await;

        // Should get a fatal error because no mint_builder in state
        let update = rx.recv().await.unwrap();
        assert_eq!(update.transaction_id, 14);
        assert_eq!(update.status, TransactionStatus::Failed);
    }

    /// `MintNotInitialized` with no transaction_id means there is nothing to report to storage;
    /// `send_fatal_error` must be a no-op and the channel must remain empty.
    #[tokio::test]
    async fn confirmation_result_mint_not_initialized_without_transaction_id() {
        let mut state = make_sender_state();
        let (tx, mut rx) = mpsc::channel(10);
        // No transaction_id
        let ctx = TransactionContext {
            transaction_id: None,
            withdrawal_nonce: None,
            trace_id: None,
        };

        handle_confirmation_result(
            &mut state,
            Ok(ConfirmationResult::MintNotInitialized),
            Signature::new_unique(),
            None,
            &ctx,
            dummy_instruction(),
            RetryPolicy::None,
            &ExtraErrorCheckPolicy::None,
            &tx,
        )
        .await;

        // No transaction_id → send_fatal_error sends nothing
        drop(tx);
        assert!(rx.recv().await.is_none());
    }

    /// When the per-nonce retry counter has already reached the maximum, `send_and_confirm`
    /// must short-circuit immediately with a Failed status mentioning "retries".
    #[tokio::test]
    async fn send_and_confirm_max_retries_exceeded_sends_fatal_error() {
        let mut state = make_sender_state();
        // Pre-fill retry_counts to be at max
        state.retry_counts.insert(5, 3);
        state.retry_max_attempts = 3;

        let (tx, mut rx) = mpsc::channel(10);
        let ctx = TransactionContext {
            transaction_id: Some(20),
            withdrawal_nonce: Some(5),
            trace_id: None,
        };

        send_and_confirm(
            &mut state,
            dummy_instruction(),
            None,
            &ctx,
            RetryPolicy::Idempotent,
            &ExtraErrorCheckPolicy::None,
            &tx,
        )
        .await;

        let update = rx.recv().await.unwrap();
        assert_eq!(update.transaction_id, 20);
        assert_eq!(update.status, TransactionStatus::Failed);
        assert!(update
            .error_message
            .as_deref()
            .unwrap_or("")
            .contains("retries"));
    }

    /// A `Confirmed` result must emit `Completed` with the on-chain signature stored as
    /// `counterpart_signature`, confirming the happy-path status-update flow.
    #[tokio::test]
    async fn confirmation_result_confirmed_sends_completed_status() {
        let mut state = make_sender_state();
        state.smt_state = Some(super::super::types::SenderSMTState {
            smt_state: crate::operator::utils::smt_util::SmtState::new(0),
            nonce_to_builder: HashMap::new(),
        });
        let (tx, mut rx) = mpsc::channel(10);
        let ctx = TransactionContext {
            transaction_id: Some(30),
            withdrawal_nonce: Some(2),
            trace_id: Some("trace-confirmed".to_string()),
        };
        let sig = Signature::new_unique();

        handle_confirmation_result(
            &mut state,
            Ok(ConfirmationResult::Confirmed),
            sig,
            None,
            &ctx,
            dummy_instruction(),
            RetryPolicy::Idempotent,
            &ExtraErrorCheckPolicy::None,
            &tx,
        )
        .await;

        let update = rx.recv().await.unwrap();
        assert_eq!(update.transaction_id, 30);
        assert_eq!(update.status, TransactionStatus::Completed);
        assert_eq!(
            update.counterpart_signature.as_deref(),
            Some(sig.to_string().as_str())
        );
    }

    /// `InvalidSmtProof` without a nonce means there is no builder to regenerate a proof with,
    /// so the transaction must immediately fail rather than attempt a retry.
    #[tokio::test]
    async fn confirmation_result_invalid_smt_proof_no_nonce_sends_fatal_error() {
        let mut state = make_sender_state();
        let (tx, mut rx) = mpsc::channel(10);
        let ctx = TransactionContext {
            transaction_id: Some(15),
            withdrawal_nonce: None, // No nonce → rebuild_with_regenerated_proof returns None
            trace_id: None,
        };

        handle_confirmation_result(
            &mut state,
            Ok(ConfirmationResult::Failed(Some(
                ContraEscrowProgramError::InvalidSmtProof,
            ))),
            Signature::new_unique(),
            None,
            &ctx,
            dummy_instruction(),
            RetryPolicy::None,
            &ExtraErrorCheckPolicy::None,
            &tx,
        )
        .await;

        let update = rx.recv().await.unwrap();
        assert_eq!(update.transaction_id, 15);
        assert_eq!(update.status, TransactionStatus::Failed);
    }
}
