use crate::operator::utils::instruction_util::{
    mint_idempotency_memo, InitializeMintBuilder, MintToBuilderWithTxnId, TransactionBuilder,
};
use crate::operator::utils::transaction_util::{check_transaction_status, ConfirmationResult};
use crate::operator::{sign_and_send_transaction, SignerUtil};
use solana_keychain::SolanaSigner;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signature;
use solana_transaction_status::{
    EncodedTransaction, UiInstruction, UiMessage, UiParsedInstruction,
};
use std::str::FromStr;
use tracing::{error, info, warn};

use super::types::{InstructionWithSigners, SenderState};

const IDEMPOTENCY_SIGNATURE_LOOKBACK_LIMIT: usize = 1000;

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

/// Check recent ATA signatures for an already-confirmed mint carrying this transaction's
/// deterministic idempotency memo.
pub(super) async fn find_existing_mint_signature(
    state: &SenderState,
    builder_with_txn_id: &MintToBuilderWithTxnId,
) -> Option<Signature> {
    let transaction_id = builder_with_txn_id.txn_id as i64;
    let recipient_ata = builder_with_txn_id.builder.get_recipient_ata()?;
    let expected_memo = mint_idempotency_memo(transaction_id);

    let signatures = match state
        .rpc_client
        .get_signatures_for_address(&recipient_ata, IDEMPOTENCY_SIGNATURE_LOOKBACK_LIMIT)
        .await
    {
        Ok(signatures) => signatures,
        Err(e) => {
            warn!(
                "Failed idempotency lookup for transaction_id {} on {}: {}",
                transaction_id, recipient_ata, e
            );
            return None;
        }
    };

    for signature_status in signatures {
        if signature_status.err.is_some() {
            continue;
        }

        let memo = match signature_status.memo.as_deref() {
            Some(memo) if memo_matches(memo, &expected_memo) => memo,
            _ => continue,
        };

        let signature = match Signature::from_str(&signature_status.signature) {
            Ok(signature) => signature,
            Err(e) => {
                warn!(
                    "Skipping invalid signature returned by RPC during idempotency check: {} ({})",
                    signature_status.signature, e
                );
                continue;
            }
        };

        let transaction = match state.rpc_client.get_transaction(&signature).await {
            Ok(transaction) => transaction,
            Err(e) => {
                warn!(
                    "Failed to fetch transaction {} for idempotency confirmation: {}",
                    signature, e
                );
                continue;
            }
        };

        if transaction_succeeded(&transaction) && transaction_has_memo(&transaction, &expected_memo)
        {
            info!(
                "Skipping resend for transaction_id {}: found existing confirmed mint {} with memo {}",
                transaction_id, signature, memo
            );
            return Some(signature);
        }
    }

    None
}

fn transaction_succeeded(
    transaction: &solana_transaction_status::EncodedConfirmedTransactionWithStatusMeta,
) -> bool {
    transaction
        .transaction
        .meta
        .as_ref()
        .is_some_and(|meta| meta.err.is_none())
}

fn transaction_has_memo(
    transaction: &solana_transaction_status::EncodedConfirmedTransactionWithStatusMeta,
    expected_memo: &str,
) -> bool {
    let EncodedTransaction::Json(ui_transaction) = &transaction.transaction.transaction else {
        return false;
    };

    match &ui_transaction.message {
        UiMessage::Parsed(parsed_message) => parsed_message
            .instructions
            .iter()
            .any(|instruction| instruction_has_memo(instruction, expected_memo)),
        UiMessage::Raw(raw_message) => raw_message.instructions.iter().any(|instruction| {
            let program_id = raw_message
                .account_keys
                .get(instruction.program_id_index as usize)
                .map(|key| key.as_str());

            program_id.is_some_and(is_memo_program_id)
                && bs58::decode(&instruction.data)
                    .into_vec()
                    .map(|memo_data| memo_data == expected_memo.as_bytes())
                    .unwrap_or(false)
        }),
    }
}

fn instruction_has_memo(instruction: &UiInstruction, expected_memo: &str) -> bool {
    match instruction {
        UiInstruction::Compiled(_) => false,
        UiInstruction::Parsed(UiParsedInstruction::Parsed(parsed_instruction)) => {
            is_memo_program_id(&parsed_instruction.program_id)
                && parsed_instruction.parsed.as_str() == Some(expected_memo)
        }
        UiInstruction::Parsed(UiParsedInstruction::PartiallyDecoded(partially_decoded)) => {
            is_memo_program_id(&partially_decoded.program_id)
                && bs58::decode(&partially_decoded.data)
                    .into_vec()
                    .map(|memo_data| memo_data == expected_memo.as_bytes())
                    .unwrap_or(false)
        }
    }
}

fn is_memo_program_id(program_id: &str) -> bool {
    Pubkey::from_str(program_id)
        .map(|pubkey| pubkey == spl_memo::id() || pubkey == spl_memo::v1::id())
        .unwrap_or(false)
}

fn memo_matches(returned_memo: &str, expected_memo: &str) -> bool {
    returned_memo
        .split("; ")
        .any(|memo| strip_memo_length_prefix(memo) == expected_memo)
}

fn strip_memo_length_prefix(memo: &str) -> &str {
    let Some(stripped) = memo.strip_prefix('[') else {
        return memo;
    };

    let Some((length, value)) = stripped.split_once("] ") else {
        return memo;
    };

    if length.chars().all(|c| c.is_ascii_digit()) {
        value
    } else {
        memo
    }
}

/// Cleanup mint builder cache when transaction completes or fails
pub(super) fn cleanup_mint_builder(state: &mut SenderState, transaction_id: Option<i64>) {
    if let Some(txn_id) = transaction_id {
        state.mint_builders.remove(&txn_id);
    }
}

#[cfg(test)]
mod tests {
    use super::{memo_matches, strip_memo_length_prefix};

    #[test]
    fn strip_memo_length_prefix_handles_formatted_values() {
        assert_eq!(
            strip_memo_length_prefix("[12] contra:mint-idempotency:42"),
            "contra:mint-idempotency:42"
        );
        assert_eq!(
            strip_memo_length_prefix("contra:mint-idempotency:42"),
            "contra:mint-idempotency:42"
        );
    }

    #[test]
    fn memo_matches_handles_plain_and_formatted_values() {
        let expected = "contra:mint-idempotency:99";

        assert!(memo_matches(expected, expected));
        assert!(memo_matches("[27] contra:mint-idempotency:99", expected));
        assert!(memo_matches(
            "[5] hello; [27] contra:mint-idempotency:99",
            expected
        ));
        assert!(!memo_matches("[5] hello", expected));
    }
}
