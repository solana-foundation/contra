use crate::channel_utils::send_guaranteed;
use crate::error::{OperatorError, ProgramError};
use crate::operator::utils::instruction_util::TransactionBuilder;
use crate::operator::utils::transaction_util::{check_transaction_status, ConfirmationResult};
use crate::operator::{sign_and_send_transaction, ExtraErrorCheckPolicy, RetryPolicy};
use crate::storage::common::models::TransactionStatus;
use chrono::Utc;
use contra_escrow_program_client::errors::ContraEscrowProgramError;
use solana_keychain::SolanaSigner;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::signature::Signature;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use super::mint::{
    cleanup_mint_builder, find_existing_mint_signature, try_jit_mint_initialization,
};
use super::proof::{cleanup_failed_transaction, rebuild_with_regenerated_proof};
use super::types::{InstructionWithSigners, SenderState, TransactionStatusUpdate};

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
                    builder_with_txn_id.txn_id as i64,
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
    let transaction_id = tx_builder.transaction_id();
    let withdrawal_nonce = tx_builder.withdrawal_nonce();
    let retry_policy = tx_builder.retry_policy();
    let compute_unit_price = tx_builder.compute_unit_price();
    let extra_error_checks_policy = &tx_builder.extra_error_checks_policy();

    if let TransactionBuilder::Mint(builder_with_txn_id) = &tx_builder {
        if let Some(existing_signature) =
            find_existing_mint_signature(state, builder_with_txn_id).await
        {
            handle_success(
                state,
                Some(builder_with_txn_id.txn_id as i64),
                None,
                existing_signature,
                storage_tx,
            )
            .await;
            return;
        }
    }

    match state.handle_transaction_builder(tx_builder.clone()).await {
        Ok(instruction) => {
            info!("Transaction instruction ready for submission");
            send_and_confirm(
                state,
                instruction,
                compute_unit_price,
                transaction_id,
                withdrawal_nonce,
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
            // ResetSmtRoot is queued in pending_rotation, will be processed when ready
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
                    nonce,
                    builder_with_nonce.transaction_id,
                    builder_with_nonce.builder,
                ));
            } else {
                error!("TreeIndexMismatch for non-ReleaseFunds transaction");
            }
        }
        Err(e) => {
            error!("Failed to build transaction: {}", e);
            send_fatal_error(storage_tx, transaction_id, &e.to_string()).await;
        }
    }
}

#[allow(clippy::too_many_arguments)]
/// Sign, send, confirm, and handle the result
pub(super) async fn send_and_confirm(
    state: &mut SenderState,
    instruction: InstructionWithSigners,
    compute_unit_price: Option<u64>,
    transaction_id: Option<i64>,
    withdrawal_nonce: Option<u64>,
    retry_policy: RetryPolicy,
    extra_error_checks_policy: &ExtraErrorCheckPolicy,
    storage_tx: &mpsc::Sender<TransactionStatusUpdate>,
) {
    // Check retry limit - only for idempotent operations that can be retried at sender level
    if let Some(nonce) = withdrawal_nonce {
        match retry_policy {
            RetryPolicy::Idempotent => {
                let attempts = state.retry_counts.get(&nonce).copied().unwrap_or(0);
                if attempts >= state.retry_max_attempts {
                    error!(
                        "Max retries ({}) exceeded for withdrawal_nonce {}",
                        state.retry_max_attempts, nonce
                    );
                    send_fatal_error(storage_tx, transaction_id, "Max retries exceeded").await;
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
                // Non-idempotent operations: only one attempt at sender level
                // (RPC layer may retry the same transaction, which is safe)
                info!("Sending non-idempotent transaction - single sender-level attempt");
            }
        }
    }

    match sign_and_send_transaction(state.rpc_client.clone(), instruction.clone(), retry_policy)
        .await
    {
        Ok(signature) => {
            info!("Transaction sent with signature: {}", signature);

            let commitment_config = CommitmentConfig::confirmed();

            // Retry logic (line 627) handles polling via recursive send_and_confirm calls
            let result = check_transaction_status(
                state.rpc_client.clone(),
                &signature,
                commitment_config,
                extra_error_checks_policy,
            )
            .await;

            handle_confirmation_result(
                state,
                result,
                signature,
                compute_unit_price,
                transaction_id,
                withdrawal_nonce,
                instruction,
                retry_policy,
                extra_error_checks_policy,
                storage_tx,
            )
            .await;
        }
        Err(e) => {
            error!("Failed to send transaction: {}", e);
            cleanup_failed_transaction(state, withdrawal_nonce);
            send_fatal_error(storage_tx, transaction_id, &e.to_string()).await;
        }
    }
}

#[allow(clippy::too_many_arguments)]
/// Route confirmation results to appropriate handlers
fn handle_confirmation_result<'a>(
    state: &'a mut SenderState,
    result: Result<ConfirmationResult, crate::error::TransactionError>,
    signature: Signature,
    compute_unit_price: Option<u64>,
    transaction_id: Option<i64>,
    withdrawal_nonce: Option<u64>,
    instruction: InstructionWithSigners,
    retry_policy: RetryPolicy,
    extra_error_checks_policy: &'a ExtraErrorCheckPolicy,
    storage_tx: &'a mpsc::Sender<TransactionStatusUpdate>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        match result {
            Ok(ConfirmationResult::Confirmed) => {
                handle_success(
                    state,
                    transaction_id,
                    withdrawal_nonce,
                    signature,
                    storage_tx,
                )
                .await;
            }
            Ok(ConfirmationResult::Failed(Some(ContraEscrowProgramError::InvalidSmtProof))) => {
                warn!("InvalidSmtProof - removing nonce and rebuilding with fresh proof");
                // Remove nonce from SMT so rebuild can re-insert with fresh proof
                if let (Some(nonce), Some(ref mut smt_state)) =
                    (withdrawal_nonce, state.smt_state.as_mut())
                {
                    smt_state.smt_state.remove_nonce(nonce);
                }
                if let Some(new_instruction) =
                    rebuild_with_regenerated_proof(state, withdrawal_nonce, instruction).await
                {
                    send_and_confirm(
                        state,
                        new_instruction,
                        compute_unit_price,
                        transaction_id,
                        withdrawal_nonce,
                        retry_policy,
                        extra_error_checks_policy,
                        storage_tx,
                    )
                    .await;
                } else {
                    cleanup_failed_transaction(state, withdrawal_nonce);
                    send_fatal_error(storage_tx, transaction_id, "Failed to rebuild proof").await;
                }
            }
            Ok(ConfirmationResult::Failed(Some(
                ContraEscrowProgramError::InvalidTransactionNonceForCurrentTreeIndex,
            ))) => {
                error!("InvalidTransactionNonce - fatal error");
                cleanup_failed_transaction(state, withdrawal_nonce);
                send_fatal_error(storage_tx, transaction_id, "Invalid nonce for tree index").await;
            }
            Ok(ConfirmationResult::MintNotInitialized) => {
                // Mint account not initialized - attempt JIT initialization
                if let Some(txn_id) = transaction_id {
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
                                transaction_id,
                                withdrawal_nonce,
                                retry_policy,
                                extra_error_checks_policy,
                                storage_tx,
                            )
                            .await;
                        } else {
                            // JIT failed - fatal error
                            cleanup_failed_transaction(state, withdrawal_nonce);
                            send_fatal_error(
                                storage_tx,
                                transaction_id,
                                "Mint initialization failed",
                            )
                            .await;
                        }
                    } else {
                        // Not a Mint transaction but got MintNotInitialized - shouldn't happen
                        error!("MintNotInitialized error for non-Mint transaction");
                        cleanup_failed_transaction(state, withdrawal_nonce);
                        send_fatal_error(storage_tx, transaction_id, "Unexpected mint error").await;
                    }
                } else {
                    // No transaction_id - fatal error
                    error!("MintNotInitialized error without transaction_id");
                    cleanup_failed_transaction(state, withdrawal_nonce);
                    send_fatal_error(storage_tx, transaction_id, "Mint initialization failed")
                        .await;
                }
            }
            Ok(ConfirmationResult::Retry) => {
                // Transaction was sent but confirmation polling failed
                // (either single timeout or exhausted all retry attempts)
                // Status is UNKNOWN - transaction might still be processing on-chain
                match retry_policy {
                    RetryPolicy::None => {
                        // Non-idempotent operations: Cannot safely retry because transaction
                        // might still be processing.
                        error!("Confirmation failed for non-idempotent operation - status unknown, cannot retry");
                        cleanup_failed_transaction(state, withdrawal_nonce);
                        send_fatal_error(
                            storage_tx,
                            transaction_id,
                            "Confirmation failed - transaction status unknown, unsafe to retry",
                        )
                        .await;
                    }
                    RetryPolicy::Idempotent => {
                        // Idempotent operations: Safe to retry because nonce prevents duplicates
                        // even if the original transaction eventually processes
                        warn!("Confirmation failed for idempotent operation - retrying (nonce protects against duplicates)");
                        send_and_confirm(
                            state,
                            instruction,
                            compute_unit_price,
                            transaction_id,
                            withdrawal_nonce,
                            retry_policy,
                            extra_error_checks_policy,
                            storage_tx,
                        )
                        .await;
                    }
                }
            }
            Ok(ConfirmationResult::Failed(program_error)) => {
                error!("Other program error: {:?}", program_error);
                cleanup_failed_transaction(state, withdrawal_nonce);
                send_fatal_error(storage_tx, transaction_id, &format!("{:?}", program_error)).await;
            }
            Err(e) => {
                error!("Confirmation error: {}", e);
                cleanup_failed_transaction(state, withdrawal_nonce);
                send_fatal_error(storage_tx, transaction_id, &e.to_string()).await;
            }
        }
    })
}

/// Handle successful transaction confirmation
pub(super) async fn handle_success(
    state: &mut SenderState,
    transaction_id: Option<i64>,
    withdrawal_nonce: Option<u64>,
    signature: Signature,
    storage_tx: &mpsc::Sender<TransactionStatusUpdate>,
) {
    info!("✅ Transaction confirmed: {}", signature);

    // Handle ReleaseFunds (withdrawal nonce-based) transactions
    if let (Some(nonce), Some(ref mut smt_state)) = (withdrawal_nonce, state.smt_state.as_mut()) {
        smt_state.nonce_to_builder.remove(&nonce);
        state.retry_counts.remove(&nonce);
        info!("Cleaned up state for withdrawal_nonce {}", nonce);

        // Send success to storage (using transaction_id for DB update)
        if let Some(txn_id) = transaction_id {
            send_guaranteed(
                storage_tx,
                TransactionStatusUpdate {
                    transaction_id: txn_id,
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
    else if let Some(transaction_id) = transaction_id {
        info!("Updating database for transaction_id {}", transaction_id);

        cleanup_mint_builder(state, Some(transaction_id));

        // Send success to storage
        send_guaranteed(
            storage_tx,
            TransactionStatusUpdate {
                transaction_id,
                status: TransactionStatus::Completed,
                counterpart_signature: Some(signature.to_string()),
                processed_at: Some(Utc::now()),
                error_message: None,
            },
            "transaction status update",
        )
        .await
        .ok(); // Log error but don't fail
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
    transaction_id: Option<i64>,
    error_msg: &str,
) {
    if let Some(transaction_id) = transaction_id {
        send_guaranteed(
            storage_tx,
            TransactionStatusUpdate {
                transaction_id,
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
