use {
    super::types::StoredTransaction,
    base64::{engine::general_purpose::STANDARD, Engine},
    solana_sdk::{
        account::ReadableAccount, clock::UnixTimestamp, message::v0::LoadedAddresses,
        transaction::SanitizedTransaction,
    },
    solana_svm::transaction_processing_result::ProcessedTransaction,
    solana_transaction_status::{
        TransactionStatusMeta, UiTransactionEncoding, UiTransactionStatusMeta,
    },
    solana_transaction_status_client_types::InnerInstructions,
    tracing::info,
};

pub fn get_stored_transaction(
    transaction: &SanitizedTransaction,
    slot: u64,
    block_time: UnixTimestamp,
    processed: &ProcessedTransaction,
) -> StoredTransaction {
    info!("Stored transaction: {:?}", processed);

    let meta = match processed {
        ProcessedTransaction::Executed(executed) => {
            let details = &executed.execution_details;
            TransactionStatusMeta {
                status: details.status.clone(),
                fee: executed.loaded_transaction.fee_details.total_fee(),
                pre_balances: executed
                    .loaded_transaction
                    .accounts
                    .iter()
                    .map(|(_, account)| account.lamports())
                    .collect(),
                post_balances: executed
                    .loaded_transaction
                    .accounts
                    .iter()
                    .map(|(_, account)| account.lamports())
                    .collect(),
                inner_instructions: details.inner_instructions.as_ref().map(|inner| {
                    inner
                        .iter()
                        .enumerate()
                        .map(|(index, instructions)| InnerInstructions {
                            index: index as u8,
                            instructions: instructions
                                .iter()
                                .map(|ii| {
                                    solana_transaction_status_client_types::InnerInstruction {
                                        instruction: ii.instruction.clone(),
                                        stack_height: Some(ii.stack_height as u32),
                                    }
                                })
                                .collect(),
                        })
                        .collect()
                }),
                log_messages: details.log_messages.clone(),
                pre_token_balances: None,
                post_token_balances: None,
                rewards: None,
                loaded_addresses: LoadedAddresses::default(),
                return_data: details.return_data.clone(),
                compute_units_consumed: Some(details.executed_units),
                cost_units: Some(executed.loaded_transaction.loaded_accounts_data_size as u64),
            }
        }
        ProcessedTransaction::FeesOnly(fees_only) => TransactionStatusMeta {
            status: Err(fees_only.load_error.clone()),
            fee: fees_only.fee_details.total_fee(),
            pre_balances: vec![],
            post_balances: vec![],
            inner_instructions: None,
            log_messages: None,
            pre_token_balances: None,
            post_token_balances: None,
            rewards: None,
            loaded_addresses: LoadedAddresses::default(),
            return_data: None,
            compute_units_consumed: None,
            cost_units: None,
        },
    };

    StoredTransaction {
        slot,
        block_time,
        transaction: transaction.to_versioned_transaction(),
        meta: UiTransactionStatusMeta::from(meta),
    }
}

pub fn encode_transaction_data(data: &[u8], encoding: UiTransactionEncoding) -> String {
    match encoding {
        UiTransactionEncoding::Base58 => bs58::encode(data).into_string(),
        UiTransactionEncoding::Base64 | UiTransactionEncoding::Binary => STANDARD.encode(data),
        UiTransactionEncoding::Json => STANDARD.encode(data),
        UiTransactionEncoding::JsonParsed => STANDARD.encode(data),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_base58() {
        let data = b"hello";
        let encoded = encode_transaction_data(data, UiTransactionEncoding::Base58);
        assert_eq!(encoded, bs58::encode(b"hello").into_string());
        // Verify roundtrip
        let decoded = bs58::decode(&encoded).into_vec().unwrap();
        assert_eq!(decoded, b"hello");
    }

    #[test]
    fn test_encode_base64() {
        let data = b"hello";
        let encoded = encode_transaction_data(data, UiTransactionEncoding::Base64);
        assert_eq!(encoded, STANDARD.encode(b"hello"));
        // Verify roundtrip
        let decoded = STANDARD.decode(&encoded).unwrap();
        assert_eq!(decoded, b"hello");
    }

    #[test]
    fn test_encode_binary_same_as_base64() {
        let data = b"test data";
        let base64 = encode_transaction_data(data, UiTransactionEncoding::Base64);
        let binary = encode_transaction_data(data, UiTransactionEncoding::Binary);
        assert_eq!(base64, binary);
    }

    #[test]
    fn test_encode_json_uses_base64() {
        let data = b"json data";
        let json = encode_transaction_data(data, UiTransactionEncoding::Json);
        let base64 = encode_transaction_data(data, UiTransactionEncoding::Base64);
        assert_eq!(json, base64);
    }

    #[test]
    fn test_encode_json_parsed_uses_base64() {
        let data = b"parsed data";
        let parsed = encode_transaction_data(data, UiTransactionEncoding::JsonParsed);
        let base64 = encode_transaction_data(data, UiTransactionEncoding::Base64);
        assert_eq!(parsed, base64);
    }
}
