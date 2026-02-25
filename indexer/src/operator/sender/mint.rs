use crate::operator::utils::instruction_util::{
    mint_idempotency_memo, InitializeMintBuilder, MintToBuilderWithTxnId, TransactionBuilder,
};
use crate::operator::utils::transaction_util::{check_transaction_status, ConfirmationResult};
use crate::operator::{sign_and_send_transaction, SignerUtil};
use serde_json::Value;
use solana_keychain::SolanaSigner;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signature;
use solana_transaction_status::{
    EncodedTransaction, ParsedInstruction, UiCompiledInstruction, UiInstruction, UiMessage,
    UiParsedInstruction, UiParsedMessage, UiPartiallyDecodedInstruction, UiRawMessage,
};
use std::str::FromStr;
use tracing::{error, info, warn};

use super::types::{InstructionWithSigners, SenderState};

const IDEMPOTENCY_SIGNATURE_LOOKBACK_LIMIT: usize = 1000;

#[derive(Clone, Copy, Debug)]
struct ExpectedMintInstruction {
    mint: Pubkey,
    recipient_ata: Pubkey,
    mint_authority: Pubkey,
    token_program: Pubkey,
    amount: u64,
}

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
    let expected_mint = expected_mint_instruction(transaction_id, builder_with_txn_id)?;
    let expected_memo = mint_idempotency_memo(transaction_id);

    let signatures = match state
        .rpc_client
        .get_signatures_for_address(
            &expected_mint.recipient_ata,
            IDEMPOTENCY_SIGNATURE_LOOKBACK_LIMIT,
        )
        .await
    {
        Ok(signatures) => signatures,
        Err(e) => {
            warn!(
                "Failed idempotency lookup for transaction_id {} on {}: {}",
                transaction_id, expected_mint.recipient_ata, e
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

        if transaction_matches_expected_mint(&transaction, &expected_memo, &expected_mint) {
            info!(
                "Skipping resend for transaction_id {}: found existing confirmed mint {} with memo {}",
                transaction_id, signature, memo
            );
            return Some(signature);
        }
    }

    None
}

fn expected_mint_instruction(
    transaction_id: i64,
    builder_with_txn_id: &MintToBuilderWithTxnId,
) -> Option<ExpectedMintInstruction> {
    let builder = &builder_with_txn_id.builder;

    let mint = match builder.get_mint() {
        Some(mint) => mint,
        None => {
            warn!(
                "Cannot run mint idempotency check for transaction_id {}: mint not set",
                transaction_id
            );
            return None;
        }
    };

    let recipient_ata = match builder.get_recipient_ata() {
        Some(recipient_ata) => recipient_ata,
        None => {
            warn!(
                "Cannot run mint idempotency check for transaction_id {}: recipient_ata not set",
                transaction_id
            );
            return None;
        }
    };

    let mint_authority = match builder.get_mint_authority() {
        Some(mint_authority) => mint_authority,
        None => {
            warn!(
                "Cannot run mint idempotency check for transaction_id {}: mint_authority not set",
                transaction_id
            );
            return None;
        }
    };

    let token_program = match builder.get_token_program() {
        Some(token_program) => token_program,
        None => {
            warn!(
                "Cannot run mint idempotency check for transaction_id {}: token_program not set",
                transaction_id
            );
            return None;
        }
    };

    let amount = match builder.get_amount() {
        Some(amount) => amount,
        None => {
            warn!(
                "Cannot run mint idempotency check for transaction_id {}: amount not set",
                transaction_id
            );
            return None;
        }
    };

    Some(ExpectedMintInstruction {
        mint,
        recipient_ata,
        mint_authority,
        token_program,
        amount,
    })
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

fn transaction_matches_expected_mint(
    transaction: &solana_transaction_status::EncodedConfirmedTransactionWithStatusMeta,
    expected_memo: &str,
    expected_mint: &ExpectedMintInstruction,
) -> bool {
    if !transaction_succeeded(transaction) {
        return false;
    }

    let EncodedTransaction::Json(ui_transaction) = &transaction.transaction.transaction else {
        return false;
    };

    match &ui_transaction.message {
        UiMessage::Parsed(parsed_message) => {
            parsed_message_has_signer(parsed_message, &expected_mint.mint_authority)
                && parsed_message
                    .instructions
                    .iter()
                    .any(|instruction| instruction_has_memo(instruction, expected_memo))
                && parsed_message
                    .instructions
                    .iter()
                    .any(|instruction| instruction_has_expected_mint(instruction, expected_mint))
        }
        UiMessage::Raw(raw_message) => {
            raw_message_has_signer(raw_message, &expected_mint.mint_authority)
                && raw_message.instructions.iter().any(|instruction| {
                    raw_instruction_has_memo(raw_message, instruction, expected_memo)
                })
                && raw_message.instructions.iter().any(|instruction| {
                    raw_instruction_has_expected_mint(raw_message, instruction, expected_mint)
                })
        }
    }
}

fn parsed_message_has_signer(parsed_message: &UiParsedMessage, signer: &Pubkey) -> bool {
    parsed_message
        .account_keys
        .iter()
        .any(|account| account.signer && parse_pubkey(&account.pubkey) == Some(*signer))
}

fn raw_message_has_signer(raw_message: &UiRawMessage, signer: &Pubkey) -> bool {
    raw_message
        .account_keys
        .iter()
        .position(|account| parse_pubkey(account) == Some(*signer))
        .is_some_and(|index| index < raw_message.header.num_required_signatures as usize)
}

fn raw_instruction_has_memo(
    raw_message: &UiRawMessage,
    instruction: &UiCompiledInstruction,
    expected_memo: &str,
) -> bool {
    let Some(program_id) = raw_message
        .account_keys
        .get(instruction.program_id_index as usize)
    else {
        return false;
    };

    is_memo_program_id(program_id)
        && bs58::decode(&instruction.data)
            .into_vec()
            .map(|memo_data| memo_data == expected_memo.as_bytes())
            .unwrap_or(false)
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

fn instruction_has_expected_mint(
    instruction: &UiInstruction,
    expected_mint: &ExpectedMintInstruction,
) -> bool {
    match instruction {
        UiInstruction::Compiled(_) => false,
        UiInstruction::Parsed(UiParsedInstruction::Parsed(parsed_instruction)) => {
            parsed_instruction_has_expected_mint(parsed_instruction, expected_mint)
        }
        UiInstruction::Parsed(UiParsedInstruction::PartiallyDecoded(partially_decoded)) => {
            partially_decoded_instruction_has_expected_mint(partially_decoded, expected_mint)
        }
    }
}

fn parsed_instruction_has_expected_mint(
    parsed_instruction: &ParsedInstruction,
    expected_mint: &ExpectedMintInstruction,
) -> bool {
    if parse_pubkey(&parsed_instruction.program_id) != Some(expected_mint.token_program) {
        return false;
    }

    let Some(instruction_type) = parsed_instruction
        .parsed
        .get("type")
        .and_then(Value::as_str)
    else {
        return false;
    };

    if instruction_type != "mintTo" && instruction_type != "mintToChecked" {
        return false;
    }

    let Some(info) = parsed_instruction.parsed.get("info") else {
        return false;
    };

    if parse_pubkey_field(info, "mint") != Some(expected_mint.mint)
        || parse_pubkey_field(info, "account") != Some(expected_mint.recipient_ata)
        || parse_pubkey_field(info, "mintAuthority") != Some(expected_mint.mint_authority)
    {
        return false;
    }

    let amount = match instruction_type {
        "mintTo" => parse_u64_field(info, "amount"),
        "mintToChecked" => info
            .get("tokenAmount")
            .and_then(|token_amount| parse_u64_field(token_amount, "amount")),
        _ => None,
    };

    amount == Some(expected_mint.amount)
}

fn partially_decoded_instruction_has_expected_mint(
    partially_decoded: &UiPartiallyDecodedInstruction,
    expected_mint: &ExpectedMintInstruction,
) -> bool {
    let Some(program_id) = parse_pubkey(&partially_decoded.program_id) else {
        return false;
    };

    if program_id != expected_mint.token_program {
        return false;
    }

    let Some(mint) = partially_decoded
        .accounts
        .first()
        .and_then(|account| parse_pubkey(account))
    else {
        return false;
    };
    let Some(recipient_ata) = partially_decoded
        .accounts
        .get(1)
        .and_then(|account| parse_pubkey(account))
    else {
        return false;
    };
    let Some(mint_authority) = partially_decoded
        .accounts
        .get(2)
        .and_then(|account| parse_pubkey(account))
    else {
        return false;
    };

    if mint != expected_mint.mint
        || recipient_ata != expected_mint.recipient_ata
        || mint_authority != expected_mint.mint_authority
    {
        return false;
    }

    bs58::decode(&partially_decoded.data)
        .into_vec()
        .ok()
        .and_then(|instruction_data| {
            parse_token_instruction_mint_amount(&program_id, &instruction_data)
        })
        == Some(expected_mint.amount)
}

fn raw_instruction_has_expected_mint(
    raw_message: &UiRawMessage,
    instruction: &UiCompiledInstruction,
    expected_mint: &ExpectedMintInstruction,
) -> bool {
    let Some(program_id) = raw_message
        .account_keys
        .get(instruction.program_id_index as usize)
        .and_then(|account| parse_pubkey(account))
    else {
        return false;
    };

    if program_id != expected_mint.token_program {
        return false;
    }

    let Some(mint) = instruction
        .accounts
        .first()
        .and_then(|index| raw_message.account_keys.get(*index as usize))
        .and_then(|account| parse_pubkey(account))
    else {
        return false;
    };
    let Some(recipient_ata) = instruction
        .accounts
        .get(1)
        .and_then(|index| raw_message.account_keys.get(*index as usize))
        .and_then(|account| parse_pubkey(account))
    else {
        return false;
    };
    let Some(mint_authority) = instruction
        .accounts
        .get(2)
        .and_then(|index| raw_message.account_keys.get(*index as usize))
        .and_then(|account| parse_pubkey(account))
    else {
        return false;
    };

    if mint != expected_mint.mint
        || recipient_ata != expected_mint.recipient_ata
        || mint_authority != expected_mint.mint_authority
    {
        return false;
    }

    bs58::decode(&instruction.data)
        .into_vec()
        .ok()
        .and_then(|instruction_data| {
            parse_token_instruction_mint_amount(&program_id, &instruction_data)
        })
        == Some(expected_mint.amount)
}

fn parse_pubkey(value: &str) -> Option<Pubkey> {
    Pubkey::from_str(value).ok()
}

fn parse_pubkey_field(value: &Value, field: &str) -> Option<Pubkey> {
    value
        .get(field)
        .and_then(Value::as_str)
        .and_then(parse_pubkey)
}

fn parse_u64_field(value: &Value, field: &str) -> Option<u64> {
    value
        .get(field)
        .and_then(Value::as_str)
        .and_then(|amount| amount.parse::<u64>().ok())
}

fn parse_token_instruction_mint_amount(program_id: &Pubkey, data: &[u8]) -> Option<u64> {
    if *program_id == spl_token::id() {
        return match spl_token::instruction::TokenInstruction::unpack(data).ok()? {
            spl_token::instruction::TokenInstruction::MintTo { amount }
            | spl_token::instruction::TokenInstruction::MintToChecked { amount, .. } => {
                Some(amount)
            }
            _ => None,
        };
    }

    if *program_id == spl_token_2022::id() {
        return match spl_token_2022::instruction::TokenInstruction::unpack(data).ok()? {
            spl_token_2022::instruction::TokenInstruction::MintTo { amount }
            | spl_token_2022::instruction::TokenInstruction::MintToChecked { amount, .. } => {
                Some(amount)
            }
            _ => None,
        };
    }

    None
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
    use super::{
        instruction_has_expected_mint, memo_matches, strip_memo_length_prefix,
        ExpectedMintInstruction,
    };
    use solana_sdk::pubkey::Pubkey;
    use solana_transaction_status::{ParsedInstruction, UiInstruction, UiParsedInstruction};

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

    #[test]
    fn instruction_has_expected_mint_matches_mint_to_instruction() {
        let mint = Pubkey::new_unique();
        let recipient_ata = Pubkey::new_unique();
        let mint_authority = Pubkey::new_unique();
        let amount = 123_u64;
        let expected = ExpectedMintInstruction {
            mint,
            recipient_ata,
            mint_authority,
            token_program: spl_token::id(),
            amount,
        };
        let instruction = UiInstruction::Parsed(UiParsedInstruction::Parsed(ParsedInstruction {
            program: "spl-token".to_string(),
            program_id: spl_token::id().to_string(),
            parsed: serde_json::json!({
                "type": "mintTo",
                "info": {
                    "mint": mint.to_string(),
                    "account": recipient_ata.to_string(),
                    "mintAuthority": mint_authority.to_string(),
                    "amount": amount.to_string(),
                }
            }),
            stack_height: None,
        }));

        assert!(instruction_has_expected_mint(&instruction, &expected));
    }

    #[test]
    fn instruction_has_expected_mint_rejects_amount_mismatch() {
        let mint = Pubkey::new_unique();
        let recipient_ata = Pubkey::new_unique();
        let mint_authority = Pubkey::new_unique();
        let expected = ExpectedMintInstruction {
            mint,
            recipient_ata,
            mint_authority,
            token_program: spl_token::id(),
            amount: 500_u64,
        };
        let instruction = UiInstruction::Parsed(UiParsedInstruction::Parsed(ParsedInstruction {
            program: "spl-token".to_string(),
            program_id: spl_token::id().to_string(),
            parsed: serde_json::json!({
                "type": "mintTo",
                "info": {
                    "mint": mint.to_string(),
                    "account": recipient_ata.to_string(),
                    "mintAuthority": mint_authority.to_string(),
                    "amount": "123",
                }
            }),
            stack_height: None,
        }));

        assert!(!instruction_has_expected_mint(&instruction, &expected));
    }

    #[test]
    fn instruction_has_expected_mint_matches_mint_to_checked_instruction() {
        let mint = Pubkey::new_unique();
        let recipient_ata = Pubkey::new_unique();
        let mint_authority = Pubkey::new_unique();
        let amount = 888_u64;
        let expected = ExpectedMintInstruction {
            mint,
            recipient_ata,
            mint_authority,
            token_program: spl_token::id(),
            amount,
        };
        let instruction = UiInstruction::Parsed(UiParsedInstruction::Parsed(ParsedInstruction {
            program: "spl-token".to_string(),
            program_id: spl_token::id().to_string(),
            parsed: serde_json::json!({
                "type": "mintToChecked",
                "info": {
                    "mint": mint.to_string(),
                    "account": recipient_ata.to_string(),
                    "mintAuthority": mint_authority.to_string(),
                    "tokenAmount": {
                        "amount": amount.to_string(),
                    }
                }
            }),
            stack_height: None,
        }));

        assert!(instruction_has_expected_mint(&instruction, &expected));
    }
}
