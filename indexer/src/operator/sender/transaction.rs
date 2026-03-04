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
    InstructionWithSigners, SenderState, TransactionContext, TransactionStatusUpdate,
};

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

/// Handle permanent transaction failure with automatic remint for withdrawals.
///
/// For withdrawal transactions: removes remint info from cache, runs cleanup
/// (which removes the nonce from SMT and builder caches), then attempts to
/// remint burned Contra tokens. Reports FailedReminted or Failed accordingly.
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

    cleanup_failed_transaction(state, ctx.withdrawal_nonce);

    let Some(info) = remint_info else {
        // Not a withdrawal — use normal fatal error path
        send_fatal_error(storage_tx, ctx, error_msg).await;
        return;
    };

    match attempt_remint(state, &info).await {
        Ok(signature) => {
            info!(
                "Withdrawal failed but tokens reminted successfully: {}",
                signature
            );
            if let Some(transaction_id) = ctx.transaction_id {
                if let Err(e) = send_guaranteed(
                    storage_tx,
                    TransactionStatusUpdate {
                        transaction_id,
                        trace_id: ctx.trace_id.clone(),
                        status: TransactionStatus::FailedReminted,
                        counterpart_signature: None,
                        processed_at: Some(Utc::now()),
                        error_message: Some(error_msg.to_string()),
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
            let combined = format!("{} | remint failed: {}", error_msg, remint_error);
            send_fatal_error(storage_tx, ctx, &combined).await;
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
    use crate::operator::MintCache;
    use crate::storage::common::storage::mock::MockStorage;
    use crate::storage::Storage;
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
    async fn permanent_failure_withdrawal_with_cache_attempts_remint_and_reports_combined_error() {
        ensure_test_signer();
        let mut state = make_sender_state();
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        // Populate remint cache — attempt_remint will fail (no real RPC)
        state.remint_cache.insert(5, make_remint_info(10));

        let ctx = TransactionContext {
            transaction_id: Some(10),
            withdrawal_nonce: Some(5),
            trace_id: Some("trace-10".to_string()),
        };

        handle_permanent_failure(&mut state, &ctx, &storage_tx, "release_funds failed").await;

        let update = storage_rx.try_recv().expect("should receive status update");
        assert_eq!(update.status, TransactionStatus::Failed);

        // Error should contain both original and remint failure
        let err = update.error_message.as_deref().unwrap();
        assert!(
            err.contains("release_funds failed"),
            "should contain original error: {err}"
        );
        assert!(
            err.contains("remint failed"),
            "should contain remint failure: {err}"
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
}
