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
                // Check if there are any in-flight ReleaseFunds transactions
                let has_in_flight = if let Some(ref smt_state) = self.smt_state {
                    !smt_state.nonce_to_builder.is_empty()
                } else {
                    false
                };

                if has_in_flight {
                    let in_flight_count = self
                        .smt_state
                        .as_ref()
                        .map(|s| s.nonce_to_builder.len())
                        .unwrap_or(0);

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
                    error!(
                        "Max retries ({}) exceeded for withdrawal_nonce {}",
                        state.retry_max_attempts, nonce
                    );
                    send_fatal_error(storage_tx, ctx, "Max retries exceeded").await;
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
            metrics::OPERATOR_TRANSACTIONS_SUBMITTED
                .with_label_values(&[pt, result_label])
                .inc();

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
            metrics::OPERATOR_TRANSACTIONS_SUBMITTED
                .with_label_values(&[pt, "error"])
                .inc();
            error!("Failed to send transaction: {}", e);
            cleanup_failed_transaction(state, ctx.withdrawal_nonce);
            send_fatal_error(storage_tx, ctx, &e.to_string()).await;
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
        match result {
            Ok(ConfirmationResult::Confirmed) => {
                handle_success(state, ctx, signature, storage_tx).await;
            }
            Ok(ConfirmationResult::Failed(Some(ContraEscrowProgramError::InvalidSmtProof))) => {
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
                    cleanup_failed_transaction(state, ctx.withdrawal_nonce);
                    send_fatal_error(storage_tx, ctx, "Failed to rebuild proof").await;
                }
            }
            Ok(ConfirmationResult::Failed(Some(
                ContraEscrowProgramError::InvalidTransactionNonceForCurrentTreeIndex,
            ))) => {
                error!("InvalidTransactionNonce - fatal error");
                cleanup_failed_transaction(state, ctx.withdrawal_nonce);
                send_fatal_error(storage_tx, ctx, "Invalid nonce for tree index").await;
            }
            Ok(ConfirmationResult::MintNotInitialized) => {
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
                            cleanup_failed_transaction(state, ctx.withdrawal_nonce);
                            send_fatal_error(storage_tx, ctx, "Mint initialization failed").await;
                        }
                    } else {
                        error!("MintNotInitialized error for non-Mint transaction");
                        cleanup_failed_transaction(state, ctx.withdrawal_nonce);
                        send_fatal_error(storage_tx, ctx, "Unexpected mint error").await;
                    }
                } else {
                    error!("MintNotInitialized error without transaction_id");
                    cleanup_failed_transaction(state, ctx.withdrawal_nonce);
                    send_fatal_error(storage_tx, ctx, "Mint initialization failed").await;
                }
            }
            Ok(ConfirmationResult::Retry) => match retry_policy {
                RetryPolicy::None => {
                    error!("Confirmation failed for non-idempotent operation - status unknown, cannot retry");
                    cleanup_failed_transaction(state, ctx.withdrawal_nonce);
                    send_fatal_error(
                        storage_tx,
                        ctx,
                        "Confirmation failed - transaction status unknown, unsafe to retry",
                    )
                    .await;
                }
                RetryPolicy::Idempotent => {
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
                error!("Other program error: {:?}", program_error);
                cleanup_failed_transaction(state, ctx.withdrawal_nonce);
                send_fatal_error(storage_tx, ctx, &format!("{:?}", program_error)).await;
            }
            Err(e) => {
                error!("Confirmation error: {}", e);
                cleanup_failed_transaction(state, ctx.withdrawal_nonce);
                send_fatal_error(storage_tx, ctx, &e.to_string()).await;
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
            },
            "transaction status update",
        )
        .await
        .ok();
    }
}
