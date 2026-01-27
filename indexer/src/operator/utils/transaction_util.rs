use std::sync::Arc;

use crate::error::TransactionError;
use crate::operator::utils::instruction_util::RetryPolicy;
use crate::operator::ExtraErrorCheckPolicy;
use crate::operator::{sender::types::InstructionWithSigners, RpcClientWithRetry};
use contra_escrow_program_client::errors::ContraEscrowProgramError;
use solana_keychain::SolanaSigner;
use solana_sdk::compute_budget::ComputeBudgetInstruction;
use solana_sdk::instruction::InstructionError;
use solana_sdk::{
    commitment_config::CommitmentConfig, message::Message, signature::Signature,
    transaction::Transaction,
};
use tracing::{debug, warn};

const MAX_POLL_ATTEMPTS_CONFIRMATION: u32 = 5;
const POLL_INTERVAL_MS_CONFIRMATION: u64 = 1000;

/// Result of transaction confirmation
#[derive(Debug, Clone)]
pub enum ConfirmationResult {
    /// Transaction confirmed on-chain
    Confirmed,
    /// Transaction failed with optional program error from ContraEscrowProgram
    Failed(Option<ContraEscrowProgramError>),
    /// Mint account not initialized (triggers initialization)
    MintNotInitialized,
    /// Transaction couldn't be confirmed after polling max attempts
    Retry,
}

/// Prepare and sign a transaction from an instruction and recent blockhash
///
/// # Arguments
/// * `rpc_client` - RPC client for sending transactions
/// * `ix_with_signers` - Instruction and signers
/// * `retry_policy` - Controls retry behavior for transaction send
///
/// # Signers
/// * Mint: Single signer (admin) as fee payer + mint authority
/// * ReleaseFunds: Dual signers (admin as fee payer, operator for authorization)
pub async fn sign_and_send_transaction(
    rpc_client: Arc<RpcClientWithRetry>,
    mut ix_with_signers: InstructionWithSigners,
    retry_policy: RetryPolicy,
) -> Result<Signature, TransactionError> {
    if let Some(compute_unit_price) = ix_with_signers.compute_unit_price {
        let compute_budget_ix =
            ComputeBudgetInstruction::set_compute_unit_price(compute_unit_price);
        ix_with_signers.instructions.insert(0, compute_budget_ix);
    }

    // Prepend compute budget instruction if specified
    if let Some(compute_units) = ix_with_signers.compute_budget {
        let compute_budget_ix = ComputeBudgetInstruction::set_compute_unit_limit(compute_units);
        ix_with_signers.instructions.insert(0, compute_budget_ix);
    }

    let recent_blockhash = rpc_client
        .get_latest_blockhash()
        .await
        .map_err(TransactionError::Rpc)?;

    let message = Message::new_with_blockhash(
        &ix_with_signers.instructions,
        Some(&ix_with_signers.fee_payer),
        &recent_blockhash,
    );

    let mut transaction = Transaction::new_unsigned(message);

    for signer in ix_with_signers.signers.iter() {
        signer
            .sign_partial_transaction(&mut transaction)
            .await
            .map_err(TransactionError::Signer)?;
    }

    let signature = rpc_client
        .send_transaction(&transaction, retry_policy)
        .await
        .map_err(TransactionError::Rpc)?;

    Ok(signature)
}

/// Check transaction status WITH polling
///
/// Polls up to MAX_POLL_ATTEMPTS_CONFIRMATION times for transaction to land on-chain
pub async fn check_transaction_status(
    rpc_client: Arc<RpcClientWithRetry>,
    signature: &Signature,
    commitment_config: CommitmentConfig,
    extra_error_checks_policy: &ExtraErrorCheckPolicy,
) -> Result<ConfirmationResult, TransactionError> {
    debug!("Checking transaction status: {}", signature);

    let mut attempts = 0;

    while attempts < MAX_POLL_ATTEMPTS_CONFIRMATION {
        match rpc_client.get_signature_statuses(&[*signature]).await {
            Ok(response) => {
                match response.value.first().and_then(|s| s.as_ref()) {
                    Some(status) => match status.satisfies_commitment(commitment_config) {
                        true => match &status.err {
                            None => {
                                debug!("Transaction confirmed: {}", signature);
                                return Ok(ConfirmationResult::Confirmed);
                            }
                            Some(tx_err) => {
                                debug!("Transaction failed: {:?}", tx_err);
                                let error_code = parse_program_error(tx_err);

                                match extra_error_checks_policy {
                                    ExtraErrorCheckPolicy::None => {}
                                    ExtraErrorCheckPolicy::Extra(error_checks) => {
                                        for error_check in error_checks.iter() {
                                            if let Some(result) = error_check(tx_err) {
                                                return Ok(result);
                                            }
                                        }
                                    }
                                }

                                return Ok(ConfirmationResult::Failed(error_code));
                            }
                        },
                        false => {
                            debug!("Transaction not yet at commitment level: {}", signature);
                        }
                    },
                    None => {
                        debug!("Transaction not found: {}", signature);
                    }
                }

                // Continue polling after sleep
                attempts += 1;
                if attempts < MAX_POLL_ATTEMPTS_CONFIRMATION {
                    tokio::time::sleep(tokio::time::Duration::from_millis(
                        POLL_INTERVAL_MS_CONFIRMATION,
                    ))
                    .await;
                }
            }
            Err(e) => {
                warn!("RPC error checking transaction status: {}", e);
                return Err(TransactionError::Rpc(e));
            }
        }
    }

    Ok(ConfirmationResult::Retry)
}

/// Check if transaction error indicates a mint account is not initialized
///
/// Detects Solana built-in errors for uninitialized or invalid account data:
/// - InvalidAccountData: "invalid account data for instruction"
/// - UninitializedAccount: "instruction requires an initialized account"
/// - IncorrectProgramId: "incorrect program id for instruction"
pub fn is_mint_not_initialized_error(
    err: &solana_sdk::transaction::TransactionError,
) -> Option<ConfirmationResult> {
    if matches!(
        err,
        solana_sdk::transaction::TransactionError::InstructionError(
            _,
            InstructionError::InvalidAccountData
                | InstructionError::UninitializedAccount
                | InstructionError::IncorrectProgramId
        )
    ) {
        return Some(ConfirmationResult::MintNotInitialized);
    }

    None
}

/// Parse program error code from transaction error
///
/// Extracts ContraEscrowProgramError from Solana transaction errors.
/// Returns None if error is not a custom program error.
pub fn parse_program_error(
    err: &solana_sdk::transaction::TransactionError,
) -> Option<ContraEscrowProgramError> {
    match err {
        solana_sdk::transaction::TransactionError::InstructionError(
            _,
            InstructionError::Custom(code),
        ) => {
            match *code {
                12 => Some(ContraEscrowProgramError::InvalidSmtProof),
                13 => Some(ContraEscrowProgramError::InvalidTransactionNonceForCurrentTreeIndex),
                _ => None, // Ignore other program errors
            }
        }
        _ => None,
    }
}
