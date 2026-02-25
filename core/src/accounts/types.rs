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
        TransactionStatusMeta, TransactionTokenBalance, TransactionWithStatusMeta,
        UiTransactionEncoding, UiTransactionStatusMeta, VersionedTransactionWithStatusMeta,
    },
    solana_transaction_status_client_types::InnerInstruction,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredInnerInstruction {
    pub program_id_index: u8,
    pub accounts: Vec<u8>,
    pub data: Vec<u8>,
    pub stack_height: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredInnerInstructions {
    pub index: u8,
    pub instructions: Vec<StoredInnerInstruction>,
}

/// Stored transaction with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredTransaction {
    pub slot: u64,
    pub block_time: UnixTimestamp,
    pub transaction: VersionedTransaction,
    pub inner_instructions: Option<Vec<StoredInnerInstructions>>,
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
            inner_instructions: self.inner_instructions.clone().map(|inner| {
                inner
                    .into_iter()
                    .map(
                        |stored_inner| solana_transaction_status_client_types::InnerInstructions {
                            index: stored_inner.index,
                            instructions: stored_inner
                                .instructions
                                .into_iter()
                                .map(|stored_inst| InnerInstruction {
                                    instruction: CompiledInstruction {
                                        program_id_index: stored_inst.program_id_index,
                                        accounts: stored_inst.accounts,
                                        data: stored_inst.data,
                                    },
                                    stack_height: stored_inst.stack_height,
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
