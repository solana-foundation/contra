use {
    base64::Engine,
    serde::{Deserialize, Serialize},
    solana_sdk::{
        clock::UnixTimestamp, instruction::CompiledInstruction, message::v0::LoadedAddresses,
        pubkey::Pubkey, transaction::VersionedTransaction,
        transaction_context::TransactionReturnData,
    },
    solana_transaction_status::{
        ConfirmedTransactionWithStatusMeta, EncodeError, EncodedConfirmedTransactionWithStatusMeta,
        TransactionStatusMeta, TransactionTokenBalance, TransactionWithStatusMeta, UiInstruction,
        UiTransactionEncoding, UiTransactionStatusMeta, VersionedTransactionWithStatusMeta,
    },
    solana_transaction_status_client_types::InnerInstruction,
};

/// Stored transaction with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredTransaction {
    pub slot: u64,
    pub block_time: UnixTimestamp,
    pub transaction: VersionedTransaction,
    // Store as UiTransactionStatusMeta because TransactionStatusMeta does not
    // implement Serialize/Deserialize
    pub meta: UiTransactionStatusMeta,
}

impl StoredTransaction {
    pub fn transaction_with_status_meta(&self) -> TransactionWithStatusMeta {
        TransactionWithStatusMeta::Complete(VersionedTransactionWithStatusMeta {
            transaction: self.transaction.clone(),
            meta: self.ui_to_transaction_status_meta(),
        })
    }

    pub fn encoded_transaction(
        &self,
        encoding: &UiTransactionEncoding,
        max_supported_transaction_version: Option<u8>,
    ) -> Result<EncodedConfirmedTransactionWithStatusMeta, EncodeError> {
        let confirmed_tx_with_meta = ConfirmedTransactionWithStatusMeta {
            slot: self.slot,
            tx_with_meta: self.transaction_with_status_meta(),
            block_time: Some(self.block_time),
        };
        confirmed_tx_with_meta.encode(*encoding, max_supported_transaction_version)
    }

    fn ui_to_transaction_status_meta(&self) -> TransactionStatusMeta {
        TransactionStatusMeta {
            status: self.meta.status.clone(),
            fee: self.meta.fee,
            pre_balances: self.meta.pre_balances.clone(),
            post_balances: self.meta.post_balances.clone(),
            inner_instructions: self.meta.inner_instructions.clone().map(|inner| {
                inner
                    .into_iter()
                    .map(
                        |ui_inner| solana_transaction_status_client_types::InnerInstructions {
                            index: ui_inner.index,
                            instructions: ui_inner
                                .instructions
                                .into_iter()
                                .map(|ui_inst| InnerInstruction {
                                    instruction: match ui_inst {
                                        UiInstruction::Compiled(compiled) => CompiledInstruction {
                                            program_id_index: compiled.program_id_index,
                                            accounts: compiled.accounts,
                                            data: bs58::decode(&compiled.data)
                                                .into_vec()
                                                .unwrap_or_default(),
                                        },
                                        _ => panic!("Unexpected instruction type"),
                                    },
                                    stack_height: None,
                                })
                                .collect(),
                        },
                    )
                    .collect()
            }),
            log_messages: self.meta.log_messages.clone().into(),
            pre_token_balances: self.meta.pre_token_balances.clone().map(|balances| {
                balances
                    .into_iter()
                    .map(|balance| TransactionTokenBalance {
                        account_index: balance.account_index,
                        mint: balance.mint,
                        ui_token_amount: balance.ui_token_amount,
                        owner: match balance.owner {
                            solana_transaction_status_client_types::option_serializer::OptionSerializer::Some(s) => s,
                            _ => String::new(),
                        },
                        program_id: match balance.program_id {
                            solana_transaction_status_client_types::option_serializer::OptionSerializer::Some(s) => s,
                            _ => String::new(),
                        },
                    })
                    .collect()
            }),
            post_token_balances: self.meta.post_token_balances.clone().map(|balances| {
                balances
                    .into_iter()
                    .map(|balance| TransactionTokenBalance {
                        account_index: balance.account_index,
                        mint: balance.mint,
                        ui_token_amount: balance.ui_token_amount,
                        owner: match balance.owner {
                            solana_transaction_status_client_types::option_serializer::OptionSerializer::Some(s) => s,
                            _ => String::new(),
                        },
                        program_id: match balance.program_id {
                            solana_transaction_status_client_types::option_serializer::OptionSerializer::Some(s) => s,
                            _ => String::new(),
                        },
                    })
                    .collect()
            }),
            rewards: self.meta.rewards.clone().into(),
            loaded_addresses: self
                .meta
                .loaded_addresses
                .clone()
                .map(|addresses| LoadedAddresses {
                    writable: addresses
                        .writable
                        .into_iter()
                        .filter_map(|s| Pubkey::try_from(s.as_str()).ok())
                        .collect(),
                    readonly: addresses
                        .readonly
                        .into_iter()
                        .filter_map(|s| Pubkey::try_from(s.as_str()).ok())
                        .collect(),
                })
                .unwrap_or_default(),
            return_data: self.meta.return_data.clone().map(|return_data| TransactionReturnData {
                program_id: Pubkey::try_from(return_data.program_id.as_str()).unwrap_or_default(),
                data: base64::engine::general_purpose::STANDARD
                    .decode(&return_data.data.0)
                    .unwrap_or_default(),
            }),
            compute_units_consumed: self.meta.compute_units_consumed.clone().into(),
            cost_units: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_sdk::{
        message::Message,
        signature::{Keypair, Signer},
        transaction::Transaction,
    };
    use solana_system_interface::instruction as system_instruction;

    fn make_stored_transaction() -> StoredTransaction {
        let from = Keypair::new();
        let to = Pubkey::new_unique();
        let ix = system_instruction::transfer(&from.pubkey(), &to, 100);
        let msg = Message::new(&[ix], Some(&from.pubkey()));
        let tx = Transaction::new(&[&from], msg, solana_sdk::hash::Hash::default());

        StoredTransaction {
            slot: 42,
            block_time: 1_700_000_000,
            transaction: tx.into(),
            meta: UiTransactionStatusMeta {
                err: None,
                status: Ok(()),
                fee: 5000,
                pre_balances: vec![100_000, 0],
                post_balances: vec![94_900, 100],
                inner_instructions: None.into(),
                log_messages: None.into(),
                pre_token_balances: None.into(),
                post_token_balances: None.into(),
                rewards: None.into(),
                loaded_addresses: None.into(),
                return_data: None.into(),
                compute_units_consumed: Some(200).into(),
                cost_units: None.into(),
            },
        }
    }

    #[test]
    fn test_transaction_with_status_meta_complete() {
        let stored = make_stored_transaction();
        let tx_with_meta = stored.transaction_with_status_meta();

        match tx_with_meta {
            TransactionWithStatusMeta::Complete(versioned) => {
                assert_eq!(versioned.meta.fee, 5000);
                assert_eq!(versioned.meta.pre_balances, vec![100_000, 0]);
                assert_eq!(versioned.meta.post_balances, vec![94_900, 100]);
            }
            _ => panic!("Expected Complete variant"),
        }
    }

    #[test]
    fn test_ui_to_meta_basic() {
        let stored = make_stored_transaction();
        let meta = stored.ui_to_transaction_status_meta();

        assert!(meta.status.is_ok());
        assert_eq!(meta.fee, 5000);
        assert_eq!(meta.pre_balances.len(), 2);
        assert_eq!(meta.post_balances.len(), 2);
        assert_eq!(meta.compute_units_consumed, Some(200));
    }
}
