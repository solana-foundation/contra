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
    tracing::debug,
};

pub fn get_stored_transaction(
    transaction: &SanitizedTransaction,
    slot: u64,
    block_time: UnixTimestamp,
    processed: &ProcessedTransaction,
) -> StoredTransaction {
    debug!("Stored transaction: {:?}", processed);

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
        _ => STANDARD.encode(data),
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

    /// An admin-shaped mirror (accounts aligned to account_keys) produces
    /// stored-meta balance arrays whose length equals account_keys().len(),
    /// guarding the get_stored_transaction consumer.
    #[test]
    fn test_stored_transaction_admin_mirror_balances_aligned() {
        use {
            solana_sdk::{
                account::AccountSharedData,
                instruction::{AccountMeta, Instruction},
                message::Message,
                pubkey::Pubkey,
                signature::{Keypair, Signer},
                transaction::Transaction,
            },
            solana_svm::{
                account_loader::LoadedTransaction,
                transaction_execution_result::{ExecutedTransaction, TransactionExecutionDetails},
            },
            solana_svm_transaction::svm_message::SVMMessage,
            std::collections::{HashMap, HashSet},
        };

        // Admin-shaped tx: account_keys = [payer, mint, spl_token].
        let payer = Keypair::new();
        let mint = Pubkey::new_unique();
        let ix = Instruction {
            program_id: spl_token::id(),
            accounts: vec![
                AccountMeta::new(mint, false),
                AccountMeta::new(payer.pubkey(), true),
            ],
            data: vec![0u8; 35],
        };
        let msg = Message::new(&[ix], Some(&payer.pubkey()));
        let legacy = Transaction::new(&[&payer], msg, solana_sdk::hash::Hash::default());
        let tx = SanitizedTransaction::try_from_legacy_transaction(legacy, &HashSet::new()).unwrap();

        // Full mirror: one slot per account key.
        let account_keys = tx.account_keys();
        let accounts: Vec<(Pubkey, AccountSharedData)> = (0..account_keys.len())
            .map(|i| {
                (
                    account_keys.get(i).copied().unwrap(),
                    AccountSharedData::new(1, 0, &Pubkey::default()),
                )
            })
            .collect();

        let executed = ExecutedTransaction {
            loaded_transaction: LoadedTransaction {
                accounts,
                ..Default::default()
            },
            execution_details: TransactionExecutionDetails {
                status: Ok(()),
                log_messages: None,
                inner_instructions: None,
                return_data: None,
                executed_units: 0,
                accounts_data_len_delta: 0,
            },
            programs_modified_by_tx: HashMap::new(),
        };
        let processed = ProcessedTransaction::Executed(Box::new(executed));

        let stored = get_stored_transaction(&tx, 1, 0, &processed);
        let expected = account_keys.len();
        assert_eq!(stored.meta.pre_balances.len(), expected);
        assert_eq!(stored.meta.post_balances.len(), expected);
    }
}
