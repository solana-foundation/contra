use crate::operator::utils::instruction_util::{InitializeMintBuilder, TransactionBuilder};
use crate::operator::utils::transaction_util::{check_transaction_status, ConfirmationResult};
use crate::operator::{sign_and_send_transaction, SignerUtil};
use solana_keychain::SolanaSigner;
use solana_sdk::commitment_config::CommitmentConfig;
use tracing::{error, info, warn};

use super::types::{InstructionWithSigners, SenderState};

/// Attempt JIT mint initialization by sending initialize_mint transaction first
/// Returns Some(mint_to_instruction) if successful, None if failed
pub(super) async fn try_jit_mint_initialization(
    state: &mut SenderState,
    transaction_id: i64,
    instruction: InstructionWithSigners,
) -> Option<InstructionWithSigners> {
    // 1. Get cached builder
    let builder = state.mint_builders.get(&transaction_id)?.clone();

    // 2. Extract mint pubkey
    let mint = builder.get_mint()?;

    // 3. Check if mint exists on Contra
    match state.rpc_client.get_account_data(&mint).await {
        Ok(data) if !data.is_empty() => return Some(instruction),
        Ok(_) => {
            info!(
                "Mint {} not found on Contra - attempting JIT initialization",
                mint
            );
        }
        Err(e) => {
            warn!(
                "RPC error checking mint {} - assuming it doesn't exist: {}",
                mint, e
            );
            // Proceed with JIT as fail-safe
        }
    }

    // 4. Look up mint decimals from mint cache
    let Ok(mint_metadata) = state.mint_cache.get_mint_metadata(&mint).await else {
        error!("Mint {} not found in mint cache", mint);
        return None;
    };

    info!(
        "Found mint metadata: {} decimals for {}",
        mint_metadata.decimals, mint
    );

    // 5. Build InitializeMint transaction
    let admin_pubkey = SignerUtil::admin_signer().pubkey();
    let init_mint_builder = InitializeMintBuilder::new(
        mint,
        mint_metadata.decimals,
        admin_pubkey,
        state.mint_cache.get_contra_token_program(),
        admin_pubkey,
    );

    let init_tx_builder = TransactionBuilder::InitializeMint(Box::new(init_mint_builder));

    // 6. Convert to instruction using existing function
    let init_instruction = match state
        .handle_transaction_builder(init_tx_builder.clone())
        .await
    {
        Ok(ix) => ix,
        Err(e) => {
            error!("Failed to build InitializeMint instruction: {}", e);
            return None;
        }
    };

    // 7. Send transaction using existing function
    info!("Sending InitializeMint transaction for mint {}", mint);
    let sig = match sign_and_send_transaction(
        state.rpc_client.clone(),
        init_instruction,
        init_tx_builder.retry_policy(),
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            error!("Failed to send InitializeMint transaction: {}", e);
            return None;
        }
    };

    // 8. Check confirmation using existing function
    let result = match check_transaction_status(
        state.rpc_client.clone(),
        &sig,
        CommitmentConfig::confirmed(),
        &init_tx_builder.extra_error_checks_policy(),
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            error!("Failed to check InitializeMint status: {}", e);
            return None;
        }
    };

    match result {
        ConfirmationResult::Confirmed => {
            info!("InitializeMint transaction confirmed: {}", sig);
            Some(instruction)
        }
        _ => {
            error!(
                "InitializeMint transaction could not be confirmed: {:?}",
                result
            );
            None
        }
    }
}

/// Cleanup mint builder cache when transaction completes or fails
pub(super) fn cleanup_mint_builder(state: &mut SenderState, transaction_id: Option<i64>) {
    if let Some(txn_id) = transaction_id {
        state.mint_builders.remove(&txn_id);
    }
}
