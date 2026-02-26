use crate::operator::utils::instruction_util::{
    mint_idempotency_memo, InitializeMintBuilder, MintToBuilderWithTxnId, TransactionBuilder,
};
use crate::operator::utils::transaction_util::{check_transaction_status, ConfirmationResult};
use crate::operator::{
    sign_and_send_transaction, RpcClientWithRetry, SignerUtil,
    MINT_IDEMPOTENCY_SIGNATURE_LOOKBACK_LIMIT,
};
use serde_json::Value;
use solana_keychain::SolanaSigner;
use solana_rpc_client_api::client_error::ErrorKind;
use solana_rpc_client_api::request::RpcError;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signature;
use solana_transaction_status::parse_instruction::ParsedInstruction;
use solana_transaction_status::{
    EncodedTransaction, UiCompiledInstruction, UiInstruction, UiMessage, UiParsedInstruction,
    UiParsedMessage, UiPartiallyDecodedInstruction, UiRawMessage,
};
use std::str::FromStr;
use tracing::{error, info, warn};

use super::types::{InstructionWithSigners, SenderState};

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
pub async fn find_existing_mint_signature(
    rpc_client: &RpcClientWithRetry,
    builder_with_txn_id: &MintToBuilderWithTxnId,
) -> Result<Option<Signature>, String> {
    let transaction_id = builder_with_txn_id.txn_id;
    let Some(expected_mint) = expected_mint_instruction(transaction_id, builder_with_txn_id) else {
        return Ok(None);
    };
    let expected_memo = mint_idempotency_memo(transaction_id);

    let signatures = match rpc_client
        .get_signatures_for_address(
            &expected_mint.recipient_ata,
            MINT_IDEMPOTENCY_SIGNATURE_LOOKBACK_LIMIT,
        )
        .await
    {
        Ok(signatures) => signatures,
        Err(e) => {
            if is_method_not_found_error(e.as_ref()) {
                warn!(
                    "Skipping mint idempotency lookup for transaction_id {}: \
                     RPC endpoint does not support getSignaturesForAddress",
                    transaction_id
                );
                return Ok(None);
            }
            return Err(format!(
                "Failed idempotency lookup for transaction_id {} on {}: {}",
                transaction_id, expected_mint.recipient_ata, e
            ));
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

        let transaction = match rpc_client.get_transaction(&signature).await {
            Ok(transaction) => transaction,
            Err(e) => {
                return Err(format!(
                    "Failed to fetch transaction {} for idempotency confirmation: {}",
                    signature, e
                ));
            }
        };

        if transaction_matches_expected_mint(&transaction, &expected_memo, &expected_mint) {
            info!(
                "Skipping resend for transaction_id {}: found existing confirmed mint {} with memo {}",
                transaction_id, signature, memo
            );
            return Ok(Some(signature));
        }
    }

    Ok(None)
}

fn is_method_not_found_error(error: &solana_rpc_client_api::client_error::Error) -> bool {
    matches!(
        error.kind(),
        ErrorKind::RpcError(RpcError::RpcResponseError { code: -32601, .. })
    )
}

fn expected_mint_instruction(
    transaction_id: i64,
    builder_with_txn_id: &MintToBuilderWithTxnId,
) -> Option<ExpectedMintInstruction> {
    let (mint, recipient_ata, mint_authority, token_program, amount) =
        builder_with_txn_id.builder.try_as_expected_mint().or_else(|| {
            warn!(
                "Cannot run mint idempotency check for transaction_id {}: builder fields incomplete",
                transaction_id
            );
            None
        })?;
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

fn accounts_and_amount_match(
    program_id: &Pubkey,
    mint: &Pubkey,
    recipient_ata: &Pubkey,
    mint_authority: &Pubkey,
    instruction_data: &[u8],
    expected: &ExpectedMintInstruction,
) -> bool {
    *program_id == expected.token_program
        && *mint == expected.mint
        && *recipient_ata == expected.recipient_ata
        && *mint_authority == expected.mint_authority
        && parse_token_instruction_mint_amount(program_id, instruction_data)
            == Some(expected.amount)
}

fn partially_decoded_instruction_has_expected_mint(
    partially_decoded: &UiPartiallyDecodedInstruction,
    expected_mint: &ExpectedMintInstruction,
) -> bool {
    let Some(program_id) = parse_pubkey(&partially_decoded.program_id) else {
        return false;
    };
    let Some(mint) = partially_decoded
        .accounts
        .first()
        .and_then(|a| parse_pubkey(a))
    else {
        return false;
    };
    let Some(recipient_ata) = partially_decoded
        .accounts
        .get(1)
        .and_then(|a| parse_pubkey(a))
    else {
        return false;
    };
    let Some(mint_authority) = partially_decoded
        .accounts
        .get(2)
        .and_then(|a| parse_pubkey(a))
    else {
        return false;
    };
    let Ok(data) = bs58::decode(&partially_decoded.data).into_vec() else {
        return false;
    };
    accounts_and_amount_match(
        &program_id,
        &mint,
        &recipient_ata,
        &mint_authority,
        &data,
        expected_mint,
    )
}

fn raw_instruction_has_expected_mint(
    raw_message: &UiRawMessage,
    instruction: &UiCompiledInstruction,
    expected_mint: &ExpectedMintInstruction,
) -> bool {
    let Some(program_id) = raw_message
        .account_keys
        .get(instruction.program_id_index as usize)
        .and_then(|a| parse_pubkey(a))
    else {
        return false;
    };
    let Some(mint) = instruction
        .accounts
        .first()
        .and_then(|i| raw_message.account_keys.get(*i as usize))
        .and_then(|a| parse_pubkey(a))
    else {
        return false;
    };
    let Some(recipient_ata) = instruction
        .accounts
        .get(1)
        .and_then(|i| raw_message.account_keys.get(*i as usize))
        .and_then(|a| parse_pubkey(a))
    else {
        return false;
    };
    let Some(mint_authority) = instruction
        .accounts
        .get(2)
        .and_then(|i| raw_message.account_keys.get(*i as usize))
        .and_then(|a| parse_pubkey(a))
    else {
        return false;
    };
    let Ok(data) = bs58::decode(&instruction.data).into_vec() else {
        return false;
    };
    accounts_and_amount_match(
        &program_id,
        &mint,
        &recipient_ata,
        &mint_authority,
        &data,
        expected_mint,
    )
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
        .map(|pubkey| pubkey == spl_memo::id())
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
        accounts_and_amount_match, expected_mint_instruction, instruction_has_expected_mint,
        memo_matches, parse_token_instruction_mint_amount,
        partially_decoded_instruction_has_expected_mint, raw_instruction_has_expected_mint,
        strip_memo_length_prefix, transaction_matches_expected_mint, ExpectedMintInstruction,
    };
    use crate::operator::utils::instruction_util::{MintToBuilder, MintToBuilderWithTxnId};
    use solana_sdk::pubkey::Pubkey;
    use solana_transaction_status::parse_instruction::ParsedInstruction;
    use solana_transaction_status::{
        option_serializer::OptionSerializer, parse_accounts::ParsedAccount,
        EncodedConfirmedTransactionWithStatusMeta, EncodedTransaction,
        EncodedTransactionWithStatusMeta, UiCompiledInstruction, UiInstruction, UiMessage,
        UiParsedInstruction, UiParsedMessage, UiPartiallyDecodedInstruction, UiRawMessage,
        UiTransaction, UiTransactionStatusMeta,
    };

    fn make_expected() -> (Pubkey, Pubkey, Pubkey, ExpectedMintInstruction) {
        let mint = Pubkey::new_unique();
        let recipient_ata = Pubkey::new_unique();
        let mint_authority = Pubkey::new_unique();
        let expected = ExpectedMintInstruction {
            mint,
            recipient_ata,
            mint_authority,
            token_program: spl_token::id(),
            amount: 1000,
        };
        (mint, recipient_ata, mint_authority, expected)
    }

    fn build_test_transaction_parsed(
        signers: &[Pubkey],
        instructions: Vec<UiInstruction>,
        meta_err: Option<solana_sdk::transaction::TransactionError>,
    ) -> EncodedConfirmedTransactionWithStatusMeta {
        let account_keys: Vec<ParsedAccount> = signers
            .iter()
            .map(|pk| ParsedAccount {
                pubkey: pk.to_string(),
                writable: true,
                signer: true,
                source: None,
            })
            .collect();

        EncodedConfirmedTransactionWithStatusMeta {
            slot: 0,
            transaction: EncodedTransactionWithStatusMeta {
                transaction: EncodedTransaction::Json(UiTransaction {
                    signatures: vec!["sig".to_string()],
                    message: UiMessage::Parsed(UiParsedMessage {
                        account_keys,
                        recent_blockhash: "11111111111111111111111111111111".to_string(),
                        instructions,
                        address_table_lookups: None,
                    }),
                }),
                meta: Some(UiTransactionStatusMeta {
                    err: meta_err,
                    status: Ok(()),
                    fee: 5000,
                    pre_balances: vec![],
                    post_balances: vec![],
                    inner_instructions: OptionSerializer::None,
                    log_messages: OptionSerializer::None,
                    pre_token_balances: OptionSerializer::None,
                    post_token_balances: OptionSerializer::None,
                    rewards: OptionSerializer::None,
                    loaded_addresses: OptionSerializer::Skip,
                    return_data: OptionSerializer::Skip,
                    compute_units_consumed: OptionSerializer::Skip,
                    cost_units: OptionSerializer::Skip,
                }),
                version: None,
            },
            block_time: None,
        }
    }

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

    #[test]
    fn expected_mint_instruction_complete_builder() {
        let mint = Pubkey::new_unique();
        let recipient_ata = Pubkey::new_unique();
        let mint_authority = Pubkey::new_unique();
        let mut builder = MintToBuilder::new();
        builder
            .mint(mint)
            .recipient_ata(recipient_ata)
            .mint_authority(mint_authority)
            .token_program(spl_token::id())
            .amount(500);

        let builder_with_id = MintToBuilderWithTxnId {
            builder,
            txn_id: 7,
            trace_id: "test".to_string(),
        };
        let result = expected_mint_instruction(7, &builder_with_id).unwrap();
        assert_eq!(result.mint, mint);
        assert_eq!(result.recipient_ata, recipient_ata);
        assert_eq!(result.mint_authority, mint_authority);
        assert_eq!(result.token_program, spl_token::id());
        assert_eq!(result.amount, 500);
    }

    #[test]
    fn expected_mint_instruction_incomplete_builder() {
        let mut builder = MintToBuilder::new();
        builder.mint(Pubkey::new_unique());
        // missing recipient_ata, mint_authority, token_program, amount

        let builder_with_id = MintToBuilderWithTxnId {
            builder,
            txn_id: 1,
            trace_id: "test".to_string(),
        };
        assert!(expected_mint_instruction(1, &builder_with_id).is_none());
    }

    #[test]
    fn accounts_and_amount_match_all_fields() {
        let (mint, recipient_ata, mint_authority, expected) = make_expected();
        let data = spl_token::instruction::TokenInstruction::MintTo { amount: 1000 }.pack();
        assert!(accounts_and_amount_match(
            &spl_token::id(),
            &mint,
            &recipient_ata,
            &mint_authority,
            &data,
            &expected,
        ));
    }

    #[test]
    fn accounts_and_amount_match_rejects_each_field() {
        let (mint, recipient_ata, mint_authority, expected) = make_expected();
        let data = spl_token::instruction::TokenInstruction::MintTo { amount: 1000 }.pack();

        // wrong program
        assert!(!accounts_and_amount_match(
            &Pubkey::new_unique(),
            &mint,
            &recipient_ata,
            &mint_authority,
            &data,
            &expected,
        ));

        // wrong mint
        assert!(!accounts_and_amount_match(
            &spl_token::id(),
            &Pubkey::new_unique(),
            &recipient_ata,
            &mint_authority,
            &data,
            &expected,
        ));

        // wrong recipient_ata
        assert!(!accounts_and_amount_match(
            &spl_token::id(),
            &mint,
            &Pubkey::new_unique(),
            &mint_authority,
            &data,
            &expected,
        ));

        // wrong mint_authority
        assert!(!accounts_and_amount_match(
            &spl_token::id(),
            &mint,
            &recipient_ata,
            &Pubkey::new_unique(),
            &data,
            &expected,
        ));

        // wrong amount
        let wrong_data = spl_token::instruction::TokenInstruction::MintTo { amount: 9999 }.pack();
        assert!(!accounts_and_amount_match(
            &spl_token::id(),
            &mint,
            &recipient_ata,
            &mint_authority,
            &wrong_data,
            &expected,
        ));
    }

    #[test]
    fn parse_token_instruction_mint_amount_spl_token() {
        let data = spl_token::instruction::TokenInstruction::MintTo { amount: 42 }.pack();
        assert_eq!(
            parse_token_instruction_mint_amount(&spl_token::id(), &data),
            Some(42)
        );

        let data_checked = spl_token::instruction::TokenInstruction::MintToChecked {
            amount: 77,
            decimals: 6,
        }
        .pack();
        assert_eq!(
            parse_token_instruction_mint_amount(&spl_token::id(), &data_checked),
            Some(77)
        );
    }

    #[test]
    fn parse_token_instruction_mint_amount_spl_token_2022() {
        let data = spl_token_2022::instruction::TokenInstruction::MintTo { amount: 100 }.pack();
        assert_eq!(
            parse_token_instruction_mint_amount(&spl_token_2022::id(), &data),
            Some(100)
        );

        let data_checked = spl_token_2022::instruction::TokenInstruction::MintToChecked {
            amount: 200,
            decimals: 9,
        }
        .pack();
        assert_eq!(
            parse_token_instruction_mint_amount(&spl_token_2022::id(), &data_checked),
            Some(200)
        );
    }

    #[test]
    fn parse_token_instruction_mint_amount_rejects_transfer() {
        let data = spl_token::instruction::TokenInstruction::Transfer { amount: 50 }.pack();
        assert_eq!(
            parse_token_instruction_mint_amount(&spl_token::id(), &data),
            None
        );
    }

    #[test]
    fn partially_decoded_mint_happy_path() {
        let (mint, recipient_ata, mint_authority, expected) = make_expected();
        let data = spl_token::instruction::TokenInstruction::MintTo { amount: 1000 }.pack();
        let partially_decoded = UiPartiallyDecodedInstruction {
            program_id: spl_token::id().to_string(),
            accounts: vec![
                mint.to_string(),
                recipient_ata.to_string(),
                mint_authority.to_string(),
            ],
            data: bs58::encode(&data).into_string(),
            stack_height: None,
        };
        assert!(partially_decoded_instruction_has_expected_mint(
            &partially_decoded,
            &expected,
        ));
    }

    #[test]
    fn partially_decoded_mint_wrong_amount() {
        let (mint, recipient_ata, mint_authority, expected) = make_expected();
        let data = spl_token::instruction::TokenInstruction::MintTo { amount: 9999 }.pack();
        let partially_decoded = UiPartiallyDecodedInstruction {
            program_id: spl_token::id().to_string(),
            accounts: vec![
                mint.to_string(),
                recipient_ata.to_string(),
                mint_authority.to_string(),
            ],
            data: bs58::encode(&data).into_string(),
            stack_height: None,
        };
        assert!(!partially_decoded_instruction_has_expected_mint(
            &partially_decoded,
            &expected,
        ));
    }

    #[test]
    fn raw_instruction_mint_happy_path() {
        let (mint, recipient_ata, mint_authority, expected) = make_expected();
        let data = spl_token::instruction::TokenInstruction::MintTo { amount: 1000 }.pack();
        let raw_message = UiRawMessage {
            header: solana_sdk::message::MessageHeader {
                num_required_signatures: 1,
                num_readonly_signed_accounts: 0,
                num_readonly_unsigned_accounts: 0,
            },
            account_keys: vec![
                mint_authority.to_string(),
                spl_token::id().to_string(),
                mint.to_string(),
                recipient_ata.to_string(),
            ],
            recent_blockhash: "11111111111111111111111111111111".to_string(),
            instructions: vec![],
            address_table_lookups: None,
        };
        let compiled = UiCompiledInstruction {
            program_id_index: 1,
            accounts: vec![2, 3, 0],
            data: bs58::encode(&data).into_string(),
            stack_height: None,
        };
        assert!(raw_instruction_has_expected_mint(
            &raw_message,
            &compiled,
            &expected,
        ));
    }

    #[test]
    fn raw_instruction_mint_wrong_program() {
        let (mint, recipient_ata, mint_authority, expected) = make_expected();
        let data = spl_token::instruction::TokenInstruction::MintTo { amount: 1000 }.pack();
        let wrong_program = Pubkey::new_unique();
        let raw_message = UiRawMessage {
            header: solana_sdk::message::MessageHeader {
                num_required_signatures: 1,
                num_readonly_signed_accounts: 0,
                num_readonly_unsigned_accounts: 0,
            },
            account_keys: vec![
                mint_authority.to_string(),
                wrong_program.to_string(),
                mint.to_string(),
                recipient_ata.to_string(),
            ],
            recent_blockhash: "11111111111111111111111111111111".to_string(),
            instructions: vec![],
            address_table_lookups: None,
        };
        let compiled = UiCompiledInstruction {
            program_id_index: 1,
            accounts: vec![2, 3, 0],
            data: bs58::encode(&data).into_string(),
            stack_height: None,
        };
        assert!(!raw_instruction_has_expected_mint(
            &raw_message,
            &compiled,
            &expected,
        ));
    }

    #[test]
    fn transaction_matches_expected_mint_parsed_happy_path() {
        let (mint, recipient_ata, mint_authority, expected) = make_expected();
        let memo_text = "contra:mint-idempotency:42";

        let memo_ix = UiInstruction::Parsed(UiParsedInstruction::Parsed(ParsedInstruction {
            program: "spl-memo".to_string(),
            program_id: spl_memo::id().to_string(),
            parsed: serde_json::Value::String(memo_text.to_string()),
            stack_height: None,
        }));
        let mint_ix = UiInstruction::Parsed(UiParsedInstruction::Parsed(ParsedInstruction {
            program: "spl-token".to_string(),
            program_id: spl_token::id().to_string(),
            parsed: serde_json::json!({
                "type": "mintTo",
                "info": {
                    "mint": mint.to_string(),
                    "account": recipient_ata.to_string(),
                    "mintAuthority": mint_authority.to_string(),
                    "amount": "1000",
                }
            }),
            stack_height: None,
        }));

        let tx = build_test_transaction_parsed(&[mint_authority], vec![memo_ix, mint_ix], None);

        assert!(transaction_matches_expected_mint(&tx, memo_text, &expected));
    }

    #[test]
    fn transaction_matches_expected_mint_rejects_failed_tx() {
        let (mint, recipient_ata, mint_authority, expected) = make_expected();
        let memo_text = "contra:mint-idempotency:42";

        let memo_ix = UiInstruction::Parsed(UiParsedInstruction::Parsed(ParsedInstruction {
            program: "spl-memo".to_string(),
            program_id: spl_memo::id().to_string(),
            parsed: serde_json::Value::String(memo_text.to_string()),
            stack_height: None,
        }));
        let mint_ix = UiInstruction::Parsed(UiParsedInstruction::Parsed(ParsedInstruction {
            program: "spl-token".to_string(),
            program_id: spl_token::id().to_string(),
            parsed: serde_json::json!({
                "type": "mintTo",
                "info": {
                    "mint": mint.to_string(),
                    "account": recipient_ata.to_string(),
                    "mintAuthority": mint_authority.to_string(),
                    "amount": "1000",
                }
            }),
            stack_height: None,
        }));

        let tx = build_test_transaction_parsed(
            &[mint_authority],
            vec![memo_ix, mint_ix],
            Some(solana_sdk::transaction::TransactionError::AccountNotFound),
        );

        assert!(!transaction_matches_expected_mint(
            &tx, memo_text, &expected
        ));
    }

    #[test]
    fn transaction_matches_expected_mint_rejects_wrong_memo() {
        let (mint, recipient_ata, mint_authority, expected) = make_expected();
        let expected_memo = "contra:mint-idempotency:42";

        let wrong_memo_ix = UiInstruction::Parsed(UiParsedInstruction::Parsed(ParsedInstruction {
            program: "spl-memo".to_string(),
            program_id: spl_memo::id().to_string(),
            parsed: serde_json::Value::String("contra:mint-idempotency:999".to_string()),
            stack_height: None,
        }));
        let mint_ix = UiInstruction::Parsed(UiParsedInstruction::Parsed(ParsedInstruction {
            program: "spl-token".to_string(),
            program_id: spl_token::id().to_string(),
            parsed: serde_json::json!({
                "type": "mintTo",
                "info": {
                    "mint": mint.to_string(),
                    "account": recipient_ata.to_string(),
                    "mintAuthority": mint_authority.to_string(),
                    "amount": "1000",
                }
            }),
            stack_height: None,
        }));

        let tx =
            build_test_transaction_parsed(&[mint_authority], vec![wrong_memo_ix, mint_ix], None);

        assert!(!transaction_matches_expected_mint(
            &tx,
            expected_memo,
            &expected,
        ));
    }
}
