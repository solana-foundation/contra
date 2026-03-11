use crate::channel_utils::send_guaranteed;
use crate::error::{OperatorError, ProgramError};
use crate::metrics;
use crate::operator::utils::instruction_util::{
    remint_idempotency_memo, MintToBuilder, MintToBuilderWithTxnId, TransactionBuilder,
    WithdrawalRemintInfo,
};
use crate::operator::utils::transaction_util::{check_transaction_status, ConfirmationResult};
use crate::operator::{sign_and_send_transaction, ExtraErrorCheckPolicy, RetryPolicy, SignerUtil};
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
    cleanup_mint_builder, find_existing_mint_signature, find_existing_mint_signature_with_memo,
    try_jit_mint_initialization,
};
use super::proof::{cleanup_failed_transaction, rebuild_with_regenerated_proof};
use super::types::{
    InstructionWithSigners, PendingRemint, SenderState, TransactionContext, TransactionStatusUpdate,
};

use std::time::Duration;

/// Safety delay before checking finality and reminting.
/// Solana finalized ≈ 32 slots × 400ms = ~12.8s. We use 2.5× safety factor.
const FINALITY_SAFETY_DELAY: Duration = Duration::from_secs(32);

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

/// Attempt to remint burned Contra tokens back to user after permanent withdrawal failure.
/// Builds a MintTo instruction with an idempotency memo (same pattern as deposits).
/// No sender-level retry — RPC-level retries may still occur via RpcClientWithRetry.
async fn attempt_remint(
    state: &SenderState,
    info: &WithdrawalRemintInfo,
) -> Result<Signature, String> {
    let memo = remint_idempotency_memo(info.transaction_id);
    let admin_pubkey = SignerUtil::admin_signer().pubkey();

    // Build remint transaction with idempotency memo to prevent duplicate mints across restarts
    let mut builder = MintToBuilder::new();
    builder
        .mint(info.mint)
        .recipient(info.user)
        .recipient_ata(info.user_ata)
        .payer(admin_pubkey)
        .mint_authority(admin_pubkey)
        .token_program(info.token_program)
        .amount(info.amount)
        .idempotency_memo(memo.clone());

    // Check for an already-confirmed remint before sending (guards against duplicate
    // remints when the operator restarts after a successful remint but before the
    // FailedReminted status is persisted to the database).
    let builder_for_lookup = MintToBuilderWithTxnId {
        builder: builder.clone(),
        txn_id: info.transaction_id,
        trace_id: info.trace_id.clone(),
    };
    match find_existing_mint_signature_with_memo(&state.rpc_client, &builder_for_lookup, &memo)
        .await
    {
        Ok(Some(existing_signature)) => {
            info!(
                "Remint already confirmed for transaction {}: {}",
                info.transaction_id, existing_signature
            );
            return Ok(existing_signature);
        }
        Ok(None) => {}
        Err(e) => {
            warn!(
                "Remint idempotency lookup failed for transaction {}: {} — proceeding with send",
                info.transaction_id, e
            );
        }
    }

    let instructions = builder
        .instructions()
        .map_err(|e| format!("Failed to build remint instructions: {}", e))?;

    let ix = InstructionWithSigners {
        instructions,
        fee_payer: admin_pubkey,
        signers: vec![SignerUtil::admin_signer()],
        compute_unit_price: None,
        compute_budget: None,
    };

    let signature = sign_and_send_transaction(state.rpc_client.clone(), ix, RetryPolicy::None)
        .await
        .map_err(|e| format!("Failed to send remint transaction: {}", e))?;

    let result = check_transaction_status(
        state.rpc_client.clone(),
        &signature,
        CommitmentConfig::confirmed(),
        &ExtraErrorCheckPolicy::None,
    )
    .await
    .map_err(|e| format!("Failed to confirm remint transaction: {}", e))?;

    match result {
        ConfirmationResult::Confirmed => {
            info!("Remint confirmed: {}", signature);
            Ok(signature)
        }
        other => Err(format!("Remint not confirmed: {:?}", other)),
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
                },
                "transaction status update",
            )
            .await
            .ok();
            return;
        }
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

/// Maximum number of finality-check retries before giving up and sending to ManualReview.
const MAX_FINALITY_CHECK_ATTEMPTS: u32 = 3;

/// Process matured entries in the deferred remint queue.
/// Called from the sender loop tick. For each matured entry, checks whether any
/// previously sent withdrawal signature reached finalized commitment. If so, the
/// withdrawal actually succeeded and we report Completed. Otherwise we attempt remint.
pub async fn process_pending_remints(
    state: &mut SenderState,
    storage_tx: &mpsc::Sender<TransactionStatusUpdate>,
) {
    let now = Utc::now();

    // Partition: matured entries get processed, immature stay in the queue
    let mut remaining = Vec::new();
    let mut matured = Vec::new();
    for entry in state.pending_remints.drain(..) {
        if entry.deadline <= now {
            matured.push(entry);
        } else {
            remaining.push(entry);
        }
    }

    for entry in matured {
        let nonce_label = entry
            .ctx
            .withdrawal_nonce
            .map(|n| n.to_string())
            .unwrap_or_else(|| "none".to_string());

        match state
            .rpc_client
            .get_signature_statuses(&entry.signatures)
            .await
        {
            Ok(response) => {
                let mut found_finalized = false;
                for (i, status_opt) in response.value.iter().enumerate() {
                    if let Some(status) = status_opt {
                        if status.satisfies_commitment(CommitmentConfig::finalized())
                            && status.err.is_none()
                        {
                            info!(
                                "Withdrawal nonce {} actually finalized (sig: {}) — skipping remint",
                                nonce_label, entry.signatures[i]
                            );
                            if let Some(transaction_id) = entry.ctx.transaction_id {
                                send_guaranteed(
                                    storage_tx,
                                    TransactionStatusUpdate {
                                        transaction_id,
                                        trace_id: entry.ctx.trace_id.clone(),
                                        status: TransactionStatus::Completed,
                                        counterpart_signature: Some(
                                            entry.signatures[i].to_string(),
                                        ),
                                        processed_at: Some(Utc::now()),
                                        error_message: None,
                                        remint_signature: None,
                                    },
                                    "transaction status update",
                                )
                                .await
                                .ok();
                            }
                            found_finalized = true;
                            break;
                        }
                    }
                }
                if found_finalized {
                    continue;
                }
                // No sig finalized → proceed to remint
                info!(
                    "No finalized withdrawal for nonce {} — attempting remint",
                    nonce_label
                );
                execute_deferred_remint(state, &entry, storage_tx).await;
            }
            Err(e) => {
                let attempt = entry.finality_check_attempts + 1;
                if attempt >= MAX_FINALITY_CHECK_ATTEMPTS {
                    error!(
                        "Finality check for nonce {} failed after {} attempts — \
                         cannot verify withdrawal status, sending to ManualReview: {}",
                        nonce_label, attempt, e
                    );
                    if let Some(transaction_id) = entry.ctx.transaction_id {
                        send_guaranteed(
                            storage_tx,
                            TransactionStatusUpdate {
                                transaction_id,
                                trace_id: entry.ctx.trace_id.clone(),
                                status: TransactionStatus::ManualReview,
                                counterpart_signature: None,
                                processed_at: Some(Utc::now()),
                                error_message: Some(format!(
                                    "{} | finality check failed after {} attempts: {}",
                                    entry.original_error, attempt, e
                                )),
                                remint_signature: None,
                            },
                            "transaction status update",
                        )
                        .await
                        .ok();
                    }
                } else {
                    warn!(
                        "Finality check for nonce {} failed (attempt {}/{}) — \
                         re-queuing with extended deadline: {}",
                        nonce_label, attempt, MAX_FINALITY_CHECK_ATTEMPTS, e
                    );
                    remaining.push(PendingRemint {
                        finality_check_attempts: attempt,
                        deadline: Utc::now()
                            + chrono::Duration::from_std(FINALITY_SAFETY_DELAY).unwrap(),
                        ..entry
                    });
                }
            }
        }
    }

    state.pending_remints = remaining;
}

/// Execute the actual remint for a matured PendingRemint entry.
async fn execute_deferred_remint(
    state: &SenderState,
    entry: &PendingRemint,
    storage_tx: &mpsc::Sender<TransactionStatusUpdate>,
) {
    match attempt_remint(state, &entry.remint_info).await {
        Ok(signature) => {
            info!(
                "Withdrawal failed but tokens reminted successfully: {}",
                signature
            );
            if let Some(transaction_id) = entry.ctx.transaction_id {
                if let Err(e) = send_guaranteed(
                    storage_tx,
                    TransactionStatusUpdate {
                        transaction_id,
                        trace_id: entry.ctx.trace_id.clone(),
                        status: TransactionStatus::FailedReminted,
                        counterpart_signature: None,
                        processed_at: Some(Utc::now()),
                        error_message: Some(entry.original_error.clone()),
                        remint_signature: Some(signature.to_string()),
                    },
                    "transaction status update",
                )
                .await
                {
                    error!(
                        "Failed to send FailedReminted status for txn {}: {}. \
                         Remint sig {} confirmed on-chain but not recorded.",
                        transaction_id, e, signature
                    );
                }
            } else {
                error!(
                    "Remint succeeded (sig: {}) but no transaction_id to record status",
                    signature
                );
            }
        }
        Err(remint_error) => {
            error!("Remint also failed: {}", remint_error);
            let combined = format!("{} | remint failed: {}", entry.original_error, remint_error);
            if let Some(transaction_id) = entry.ctx.transaction_id {
                send_guaranteed(
                    storage_tx,
                    TransactionStatusUpdate {
                        transaction_id,
                        trace_id: entry.ctx.trace_id.clone(),
                        status: TransactionStatus::ManualReview,
                        counterpart_signature: None,
                        processed_at: Some(Utc::now()),
                        error_message: Some(combined),
                        remint_signature: None,
                    },
                    "transaction status update",
                )
                .await
                .ok();
            }
        }
    }
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
    use crate::operator::sender::types::{PendingRemint, SenderSMTState};
    use crate::operator::utils::smt_util::SmtState;
    use crate::operator::MintCache;
    use crate::storage::common::storage::mock::MockStorage;
    use crate::storage::Storage;
    use contra_escrow_program_client::instructions::ReleaseFundsBuilder;
    use std::collections::HashMap;
    use std::sync::Arc;

    use std::sync::Once;

    static INIT_TEST_SIGNER: Once = Once::new();
    fn ensure_test_signer() {
        INIT_TEST_SIGNER.call_once(|| {
            // Generate a throwaway keypair for tests that hit SignerUtil
            let kp = solana_sdk::signer::keypair::Keypair::new();
            let b58 = bs58::encode(kp.to_bytes()).into_string();
            std::env::set_var("ADMIN_SIGNER", "memory");
            std::env::set_var("ADMIN_PRIVATE_KEY", &b58);
        });
    }

    fn make_sender_state() -> SenderState {
        let mock = MockStorage::new();
        let storage = Arc::new(Storage::Mock(mock));
        let rpc = Arc::new(crate::operator::RpcClientWithRetry::with_retry_config(
            "http://localhost:8899".to_string(),
            crate::operator::RetryConfig::default(),
            solana_sdk::commitment_config::CommitmentConfig::confirmed(),
        ));
        SenderState {
            rpc_client: rpc,
            storage: storage.clone(),
            instance_pda: None,
            smt_state: None,
            retry_counts: HashMap::new(),
            mint_builders: HashMap::new(),
            mint_cache: MintCache::new(storage),
            retry_max_attempts: 3,
            rotation_retry_queue: Vec::new(),
            pending_rotation: None,
            program_type: crate::config::ProgramType::Escrow,
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

    /// Build a SenderState pointed at a custom RPC URL.
    /// Uses max_attempts=1 and minimal delays so tests don't wait on retries.
    fn make_sender_state_with_rpc(rpc_url: &str) -> SenderState {
        let mock = MockStorage::new();
        let storage = Arc::new(Storage::Mock(mock));
        let rpc = Arc::new(crate::operator::RpcClientWithRetry::with_retry_config(
            rpc_url.to_string(),
            crate::operator::RetryConfig {
                max_attempts: 1,
                base_delay: std::time::Duration::from_millis(1),
                max_delay: std::time::Duration::from_millis(1),
            },
            solana_sdk::commitment_config::CommitmentConfig::confirmed(),
        ));
        SenderState {
            rpc_client: rpc,
            storage: storage.clone(),
            instance_pda: None,
            smt_state: None,
            retry_counts: HashMap::new(),
            mint_builders: HashMap::new(),
            mint_cache: MintCache::new(storage),
            retry_max_attempts: 3,
            rotation_retry_queue: Vec::new(),
            pending_rotation: None,
            program_type: crate::config::ProgramType::Escrow,
            remint_cache: HashMap::new(),
            pending_signatures: HashMap::new(),
            pending_remints: Vec::new(),
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

    #[tokio::test]
    async fn process_pending_remints_requeues_on_rpc_error() {
        let mut state = make_sender_state();
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        // Push a matured entry — RPC will fail (no real endpoint)
        state.pending_remints.push(PendingRemint {
            ctx: TransactionContext {
                transaction_id: Some(20),
                withdrawal_nonce: Some(8),
                trace_id: Some("trace-20".to_string()),
            },
            remint_info: make_remint_info(20),
            signatures: vec![Signature::new_unique()],
            original_error: "max retries".to_string(),
            deadline: Utc::now() - chrono::Duration::seconds(1),
            finality_check_attempts: 0,
        });

        process_pending_remints(&mut state, &storage_tx).await;

        // RPC error on first attempt → re-queued, not resolved
        assert!(
            storage_rx.try_recv().is_err(),
            "should NOT send status on first RPC failure"
        );
        assert_eq!(
            state.pending_remints.len(),
            1,
            "should re-queue entry after RPC error"
        );
        assert_eq!(state.pending_remints[0].finality_check_attempts, 1);
    }

    #[tokio::test]
    async fn process_pending_remints_manual_review_after_max_rpc_failures() {
        let mut state = make_sender_state();
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        // Push entry already at max attempts — next RPC failure triggers ManualReview
        state.pending_remints.push(PendingRemint {
            ctx: TransactionContext {
                transaction_id: Some(20),
                withdrawal_nonce: Some(8),
                trace_id: Some("trace-20".to_string()),
            },
            remint_info: make_remint_info(20),
            signatures: vec![Signature::new_unique()],
            original_error: "max retries".to_string(),
            deadline: Utc::now() - chrono::Duration::seconds(1),
            finality_check_attempts: 2, // MAX_FINALITY_CHECK_ATTEMPTS - 1
        });

        process_pending_remints(&mut state, &storage_tx).await;

        let update = storage_rx.try_recv().expect("should receive status update");
        assert_eq!(update.transaction_id, 20);
        assert_eq!(
            update.status,
            TransactionStatus::ManualReview,
            "exhausted finality check retries should produce ManualReview"
        );

        let err = update.error_message.as_deref().unwrap();
        assert!(
            err.contains("finality check failed"),
            "should mention finality check failure: {err}"
        );
        assert!(
            err.contains("max retries"),
            "should contain original error: {err}"
        );

        assert!(
            state.pending_remints.is_empty(),
            "should not re-queue after max attempts"
        );
    }

    #[tokio::test]
    async fn permanent_failure_drains_remint_cache() {
        ensure_test_signer();
        let mut state = make_sender_state();
        let (storage_tx, _storage_rx) = mpsc::channel(10);

        state.remint_cache.insert(5, make_remint_info(10));
        assert!(state.remint_cache.contains_key(&5));

        let ctx = TransactionContext {
            transaction_id: Some(10),
            withdrawal_nonce: Some(5),
            trace_id: Some("trace-10".to_string()),
        };

        handle_permanent_failure(&mut state, &ctx, &storage_tx, "error").await;

        assert!(
            !state.remint_cache.contains_key(&5),
            "remint_cache entry should be consumed"
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

    // ── mixed matured/immature queue ────────────────────────────────

    /// When the pending_remints queue contains both matured entries (deadline
    /// in the past) and immature ones (deadline in the future), only the
    /// matured entries should be processed on a given tick.
    ///
    /// The immature entry must remain in the queue completely unchanged —
    /// same deadline, same attempt count. Processing it early would violate
    /// the finality window guarantee that prevents double-minting.
    #[tokio::test]
    async fn process_pending_remints_handles_mixed_matured_and_immature() {
        let mut state = make_sender_state();
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        let future_deadline = Utc::now() + chrono::Duration::seconds(600);

        // Entry 1: matured — RPC will fail (localhost unreachable), so it
        // gets re-queued with attempt=1. This is the observable side-effect
        // that proves it was processed.
        state.pending_remints.push(PendingRemint {
            ctx: TransactionContext {
                transaction_id: Some(10),
                withdrawal_nonce: Some(1),
                trace_id: Some("trace-10".to_string()),
            },
            remint_info: make_remint_info(10),
            signatures: vec![Signature::new_unique()],
            original_error: "release_funds failed".to_string(),
            deadline: Utc::now() - chrono::Duration::seconds(1),
            finality_check_attempts: 0,
        });

        // Entry 2: immature — must not be touched at all.
        state.pending_remints.push(PendingRemint {
            ctx: TransactionContext {
                transaction_id: Some(20),
                withdrawal_nonce: Some(2),
                trace_id: Some("trace-20".to_string()),
            },
            remint_info: make_remint_info(20),
            signatures: vec![Signature::new_unique()],
            original_error: "release_funds failed".to_string(),
            deadline: future_deadline,
            finality_check_attempts: 0,
        });

        process_pending_remints(&mut state, &storage_tx).await;

        // No status update yet — the matured entry's RPC failed and was re-queued,
        // the immature entry was skipped entirely.
        assert!(
            storage_rx.try_recv().is_err(),
            "no status update expected on first RPC failure"
        );

        // Both entries are still in the queue.
        assert_eq!(state.pending_remints.len(), 2);

        // The matured entry was processed: attempt counter incremented.
        let matured = state
            .pending_remints
            .iter()
            .find(|e| e.ctx.transaction_id == Some(10))
            .expect("matured entry should still be in queue");
        assert_eq!(
            matured.finality_check_attempts, 1,
            "matured entry should have attempt=1 after first RPC failure"
        );

        // The immature entry was not touched: attempt counter and deadline unchanged.
        let immature = state
            .pending_remints
            .iter()
            .find(|e| e.ctx.transaction_id == Some(20))
            .expect("immature entry should still be in queue");
        assert_eq!(
            immature.finality_check_attempts, 0,
            "immature entry must not be processed"
        );
        assert_eq!(
            immature.deadline, future_deadline,
            "immature entry deadline must be unchanged"
        );
    }

    // ── remint_cache population ─────────────────────────────────────

    #[tokio::test]
    async fn process_pending_remints_skips_immature() {
        let mut state = make_sender_state();
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        // Push an entry with a future deadline
        state.pending_remints.push(PendingRemint {
            ctx: TransactionContext {
                transaction_id: Some(30),
                withdrawal_nonce: Some(9),
                trace_id: Some("trace-30".to_string()),
            },
            remint_info: make_remint_info(30),
            signatures: vec![Signature::new_unique()],
            original_error: "timeout".to_string(),
            deadline: Utc::now() + chrono::Duration::seconds(600),
            finality_check_attempts: 0,
        });

        process_pending_remints(&mut state, &storage_tx).await;

        // Nothing should be processed
        assert!(
            storage_rx.try_recv().is_err(),
            "immature entry should not be processed"
        );
        assert_eq!(
            state.pending_remints.len(),
            1,
            "immature entry should remain in queue"
        );
    }

    // ── finality check: withdrawal actually landed ───────────────────

    /// The core anti-duplication invariant: if the original withdrawal
    /// transaction reached finality on Solana, the remint must be skipped
    /// and the transaction marked Completed instead.
    ///
    /// Skipping this check would mean reminting tokens that were already
    /// successfully withdrawn — a direct double-credit to the user.
    ///
    /// This test mocks the Solana RPC to return a finalized status for the
    /// withdrawal signature and verifies that no remint is attempted and
    /// Completed is sent with the finalized signature as the counterpart.
    #[tokio::test]
    async fn process_pending_remints_marks_completed_when_withdrawal_finalized() {
        let mut rpc_server = mockito::Server::new_async().await;
        let mut state = make_sender_state_with_rpc(&rpc_server.url());
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        let sig = Signature::new_unique();

        // Mock the Solana `getSignatureStatuses` RPC call to report the
        // withdrawal signature as finalized with no error — meaning the
        // funds did leave the escrow and reached the user's wallet.
        let _mock = rpc_server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{{
                    "jsonrpc": "2.0",
                    "result": {{
                        "context": {{"slot": 200}},
                        "value": [{{
                            "slot": 100,
                            "confirmations": null,
                            "err": null,
                            "status": {{"Ok": null}},
                            "confirmationStatus": "finalized"
                        }}]
                    }},
                    "id": 0
                }}"#,
            )
            .create_async()
            .await;

        // Push a matured entry whose deadline has already passed.
        state.pending_remints.push(PendingRemint {
            ctx: TransactionContext {
                transaction_id: Some(99),
                withdrawal_nonce: Some(7),
                trace_id: Some("trace-99".to_string()),
            },
            remint_info: make_remint_info(99),
            signatures: vec![sig],
            original_error: "release_funds failed".to_string(),
            deadline: Utc::now() - chrono::Duration::seconds(1),
            finality_check_attempts: 0,
        });

        process_pending_remints(&mut state, &storage_tx).await;

        // Must send Completed — the withdrawal landed, no remint needed.
        let update = storage_rx
            .try_recv()
            .expect("should receive Completed status");
        assert_eq!(update.transaction_id, 99);
        assert_eq!(update.status, TransactionStatus::Completed);

        // The finalized withdrawal signature must be recorded as the
        // counterpart so the DB has a pointer to the on-chain transaction.
        assert_eq!(
            update.counterpart_signature.as_deref(),
            Some(sig.to_string().as_str()),
            "counterpart_signature must be the finalized withdrawal sig"
        );

        // No second message — the remint path must not have been entered.
        assert!(
            storage_rx.try_recv().is_err(),
            "should send exactly one status update — no remint attempted"
        );

        // Entry is consumed, not re-queued.
        assert!(
            state.pending_remints.is_empty(),
            "entry should be removed from queue after Completed"
        );
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

    #[test]
    fn remint_cache_populated_from_release_funds_builder() {
        let mut state = make_sender_state();
        let info = make_remint_info(42);
        let expected_amount = info.amount;

        // Directly insert into cache as handle_transaction_builder would
        state.remint_cache.insert(7, info);

        assert!(state.remint_cache.contains_key(&7));
        assert_eq!(state.remint_cache.get(&7).unwrap().amount, expected_amount);
        assert_eq!(state.remint_cache.get(&7).unwrap().transaction_id, 42);
    }

    // ── execute_deferred_remint paths ───────────────────────────────

    /// When the finality check returns null for a withdrawal signature
    /// (transaction was dropped), `execute_deferred_remint` is called.
    /// If the remint itself also fails (RPC unreachable after the finality
    /// check mock is consumed), the combined error must be sent as ManualReview.
    ///
    /// This covers the most common production failure mode: withdrawal dropped,
    /// remint RPC unavailable → operator can't recover without intervention.
    #[tokio::test]
    async fn process_pending_remints_not_finalized_remint_fails_sends_manual_review() {
        ensure_test_signer();
        let mut rpc_server = mockito::Server::new_async().await;
        let mut state = make_sender_state_with_rpc(&rpc_server.url());
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        let sig = Signature::new_unique();

        // Finality check: null means the tx was dropped — proceed to remint.
        let _mock = rpc_server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"jsonrpc":"2.0","result":{"context":{"slot":200},"value":[null]},"id":0}"#,
            )
            .create_async()
            .await;

        state.pending_remints.push(PendingRemint {
            ctx: TransactionContext {
                transaction_id: Some(77),
                withdrawal_nonce: Some(11),
                trace_id: Some("trace-77".to_string()),
            },
            remint_info: make_remint_info(77),
            signatures: vec![sig],
            original_error: "release_funds failed".to_string(),
            deadline: Utc::now() - chrono::Duration::seconds(1),
            finality_check_attempts: 0,
        });

        process_pending_remints(&mut state, &storage_tx).await;

        // Remint fails (RPC unavailable beyond the first mock) → ManualReview.
        let update = storage_rx.try_recv().expect("should receive ManualReview");
        assert_eq!(update.transaction_id, 77);
        assert_eq!(update.status, TransactionStatus::ManualReview);

        let err = update.error_message.as_deref().unwrap();
        assert!(
            err.contains("remint failed"),
            "error should mention remint failure: {err}"
        );
        assert!(
            err.contains("release_funds failed"),
            "error should include original withdrawal error: {err}"
        );

        // Entry consumed — not re-queued after ManualReview.
        assert!(state.pending_remints.is_empty());
    }

    /// A withdrawal that reached finality but failed on-chain (err field is set)
    /// is NOT a successful withdrawal — the user's funds never left the escrow.
    /// The operator must proceed to remint, not mark Completed.
    #[tokio::test]
    async fn process_pending_remints_finalized_with_onchain_error_proceeds_to_remint() {
        ensure_test_signer();
        let mut rpc_server = mockito::Server::new_async().await;
        let mut state = make_sender_state_with_rpc(&rpc_server.url());
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        let sig = Signature::new_unique();

        // Signature is finalized but has an on-chain error → withdrawal failed.
        // `status.err.is_some()` must block the Completed path.
        let _mock = rpc_server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{{
                    "jsonrpc": "2.0",
                    "result": {{
                        "context": {{"slot": 200}},
                        "value": [{{
                            "slot": 100,
                            "confirmations": null,
                            "err": {{"InstructionError": [0, {{"Custom": 1}}]}},
                            "status": {{"Err": {{"InstructionError": [0, {{"Custom": 1}}]}}}},
                            "confirmationStatus": "finalized"
                        }}]
                    }},
                    "id": 0
                }}"#,
            )
            .create_async()
            .await;

        state.pending_remints.push(PendingRemint {
            ctx: TransactionContext {
                transaction_id: Some(88),
                withdrawal_nonce: Some(12),
                trace_id: Some("trace-88".to_string()),
            },
            remint_info: make_remint_info(88),
            signatures: vec![sig],
            original_error: "timeout".to_string(),
            deadline: Utc::now() - chrono::Duration::seconds(1),
            finality_check_attempts: 0,
        });

        process_pending_remints(&mut state, &storage_tx).await;

        // Must NOT produce Completed — on-chain error means funds never moved.
        // Remint is attempted; it fails here (no further mocks) → ManualReview.
        let update = storage_rx
            .try_recv()
            .expect("should receive a status update");
        assert_ne!(
            update.status,
            TransactionStatus::Completed,
            "finalized-with-error must NOT produce Completed — funds never left escrow"
        );
        assert_eq!(update.transaction_id, 88);
    }

    /// When a withdrawal was retried and produced multiple signatures, one of the
    /// later retry signatures may reach finality. The operator must identify which
    /// specific signature finalized and record it as the counterpart_signature.
    #[tokio::test]
    async fn process_pending_remints_second_of_two_sigs_finalized_marks_completed() {
        let mut rpc_server = mockito::Server::new_async().await;
        let mut state = make_sender_state_with_rpc(&rpc_server.url());
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        let sig1 = Signature::new_unique(); // first attempt — dropped
        let sig2 = Signature::new_unique(); // retry — finalized

        // value[0] = null (sig1 dropped), value[1] = finalized (sig2 succeeded)
        let _mock = rpc_server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{{
                    "jsonrpc": "2.0",
                    "result": {{
                        "context": {{"slot": 200}},
                        "value": [
                            null,
                            {{
                                "slot": 100,
                                "confirmations": null,
                                "err": null,
                                "status": {{"Ok": null}},
                                "confirmationStatus": "finalized"
                            }}
                        ]
                    }},
                    "id": 0
                }}"#,
            )
            .create_async()
            .await;

        state.pending_remints.push(PendingRemint {
            ctx: TransactionContext {
                transaction_id: Some(55),
                withdrawal_nonce: Some(6),
                trace_id: Some("trace-55".to_string()),
            },
            remint_info: make_remint_info(55),
            signatures: vec![sig1, sig2],
            original_error: "release_funds failed".to_string(),
            deadline: Utc::now() - chrono::Duration::seconds(1),
            finality_check_attempts: 0,
        });

        process_pending_remints(&mut state, &storage_tx).await;

        let update = storage_rx
            .try_recv()
            .expect("should receive Completed status");
        assert_eq!(update.transaction_id, 55);
        assert_eq!(update.status, TransactionStatus::Completed);

        // counterpart_signature must be sig2 (the one that actually finalized).
        assert_eq!(
            update.counterpart_signature.as_deref(),
            Some(sig2.to_string().as_str()),
            "counterpart_signature must be the finalized sig (sig2), not the dropped sig1"
        );

        assert!(
            state.pending_remints.is_empty(),
            "entry consumed after Completed"
        );
    }
}
