use std::collections::HashMap;

use solana_sdk::{account::AccountSharedData, pubkey::Pubkey};
use solana_svm::{
    account_loader::{LoadedTransaction, TransactionCheckResult},
    transaction_error_metrics::TransactionErrorMetrics,
    transaction_execution_result::{ExecutedTransaction, TransactionExecutionDetails},
    transaction_processing_result::{ProcessedTransaction, TransactionProcessingResult},
    transaction_processor::{
        LoadAndExecuteSanitizedTransactionsOutput, TransactionProcessingConfig,
        TransactionProcessingEnvironment,
    },
};
use solana_svm_callback::TransactionProcessingCallback;
use solana_svm_transaction::svm_transaction::SVMTransaction;
use solana_timings::ExecuteTimings;
use spl_token::solana_program::program_option::COption;
use spl_token::solana_program::program_pack::Pack;
use spl_token::state::Mint;
use tracing::warn;

pub(crate) const SPL_TOKEN_ID: Pubkey = spl_token::id();

// SPL Token instruction types
const INSTRUCTION_INITIALIZE_MINT: u8 = 0;

/// This VM is used to execute admin transactions
#[derive(Default)]
pub struct AdminVm {}

impl AdminVm {
    /// Creates a new SPL Token Mint account with the given parameters
    fn create_mint_account(
        decimals: u8,
        mint_authority: &[u8],
        freeze_authority: Option<&[u8]>,
    ) -> AccountSharedData {
        // Parse mint authority pubkey
        let mint_auth_pubkey =
            Pubkey::new_from_array(mint_authority.try_into().expect("Invalid mint authority"));

        // Parse freeze authority if provided
        let freeze_auth_pubkey = freeze_authority
            .map(|auth| Pubkey::new_from_array(auth.try_into().expect("Invalid freeze authority")));

        // Create the Mint struct using official SPL Token types
        let mint = Mint {
            mint_authority: COption::Some(mint_auth_pubkey),
            supply: 0,
            decimals,
            is_initialized: true,
            freeze_authority: freeze_auth_pubkey
                .map(COption::Some)
                .unwrap_or(COption::None),
        };

        // Pack the mint data using the official Pack trait
        let mut mint_data = vec![0u8; Mint::LEN];
        Mint::pack(mint, &mut mint_data).expect("Failed to pack mint");

        let mut account = AccountSharedData::new(0, Mint::LEN, &spl_token::id());
        account.set_data_from_slice(&mint_data);
        account
    }

    /// Creates an ExecutedTransaction result
    fn create_executed_transaction(
        accounts: Vec<(Pubkey, AccountSharedData)>,
    ) -> ExecutedTransaction {
        ExecutedTransaction {
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
        }
    }

    #[cfg(test)]
    pub fn test_create_mint_account(
        decimals: u8,
        mint_authority: &[u8],
        freeze_authority: Option<&[u8]>,
    ) -> AccountSharedData {
        Self::create_mint_account(decimals, mint_authority, freeze_authority)
    }

    #[cfg(test)]
    pub fn test_create_executed_transaction(
        accounts: Vec<(Pubkey, AccountSharedData)>,
    ) -> ExecutedTransaction {
        Self::create_executed_transaction(accounts)
    }

    pub fn load_and_execute_sanitized_transactions<CB: TransactionProcessingCallback>(
        &self,
        _callbacks: &CB,
        sanitized_txs: &[impl SVMTransaction],
        _check_results: Vec<TransactionCheckResult>,
        _environment: &TransactionProcessingEnvironment,
        _config: &TransactionProcessingConfig,
    ) -> LoadAndExecuteSanitizedTransactionsOutput {
        let mut processing_results: Vec<TransactionProcessingResult> = vec![];
        for tx in sanitized_txs {
            let mut accounts = vec![];
            for (program_id, instruction) in tx.program_instructions_iter() {
                match *program_id {
                    SPL_TOKEN_ID => {
                        let instruction_type = instruction.data.first().unwrap();
                        match *instruction_type {
                            INSTRUCTION_INITIALIZE_MINT => {
                                // Parse InitializeMint instruction
                                if !instruction.accounts.is_empty() && instruction.data.len() >= 34
                                {
                                    // Parse instruction data
                                    // TODO: Instruction data could be invalid,
                                    // so we need to handle it without causing a
                                    // panic
                                    let account_keys = tx.account_keys();
                                    let mint_index = instruction.accounts[0] as usize;
                                    let mint_pubkey = account_keys.get(mint_index).unwrap();

                                    // Extract parameters from instruction data
                                    let decimals = instruction.data[1];
                                    let mint_authority = &instruction.data[2..34];

                                    // Check for optional freeze authority
                                    let freeze_authority = if instruction.data.len() > 34
                                        && instruction.data[34] == 1
                                    {
                                        Some(&instruction.data[35..67])
                                    } else {
                                        None
                                    };

                                    // Create the mint account
                                    let mint_account = Self::create_mint_account(
                                        decimals,
                                        mint_authority,
                                        freeze_authority,
                                    );

                                    accounts.push((*mint_pubkey, mint_account));
                                }
                            }
                            _ => {
                                warn!(
                                    "[admin-vm] Unsupported SPL token instruction type: {}",
                                    instruction_type
                                );
                            }
                        }
                    }
                    _ => {
                        warn!("[admin-vm] Unsupported program ID: {}", program_id);
                    }
                }
            }
            // Create successful processing result
            let executed_tx = Self::create_executed_transaction(accounts);
            processing_results.push(Ok(ProcessedTransaction::Executed(Box::new(executed_tx))))
        }

        LoadAndExecuteSanitizedTransactionsOutput {
            // TODO: Not implemented
            error_metrics: TransactionErrorMetrics::default(),
            // TODO: No implemented
            execute_timings: ExecuteTimings::default(),
            // TODO: Not implemented
            balance_collector: None,
            processing_results,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_sdk::account::ReadableAccount;
    use spl_token::solana_program::program_pack::Pack;
    use spl_token::state::Mint;

    #[test]
    fn test_create_mint_account_roundtrip() {
        let authority = Pubkey::new_unique();
        let account = AdminVm::create_mint_account(6, &authority.to_bytes(), None);

        let mint = Mint::unpack(account.data()).unwrap();
        assert_eq!(mint.decimals, 6);
        assert!(mint.is_initialized);
        assert_eq!(mint.supply, 0);
        assert_eq!(mint.mint_authority, COption::Some(authority));
        assert_eq!(mint.freeze_authority, COption::None);
    }

    #[test]
    fn test_initialize_mint_with_freeze_authority() {
        let authority = Pubkey::new_unique();
        let freeze = Pubkey::new_unique();
        let account =
            AdminVm::create_mint_account(9, &authority.to_bytes(), Some(&freeze.to_bytes()));

        let mint = Mint::unpack(account.data()).unwrap();
        assert_eq!(mint.decimals, 9);
        assert_eq!(mint.freeze_authority, COption::Some(freeze));
    }

    #[test]
    fn test_create_executed_transaction_defaults() {
        let executed = AdminVm::create_executed_transaction(vec![]);
        assert!(executed.execution_details.status.is_ok());
        assert_eq!(executed.execution_details.executed_units, 0);
        assert!(executed.loaded_transaction.accounts.is_empty());
    }

    #[test]
    fn test_load_and_execute_unsupported_program() {
        let vm = AdminVm::default();
        // Build a SanitizedTransaction with a non-SPL program
        let from = solana_sdk::signature::Keypair::new();
        let to = Pubkey::new_unique();
        let tx = crate::test_helpers::create_test_sanitized_transaction(&from, &to, 100);

        let check_results = crate::processor::get_transaction_check_results(1);
        let env = solana_svm::transaction_processor::TransactionProcessingEnvironment::default();
        let config = solana_svm::transaction_processor::TransactionProcessingConfig::default();

        // Use a dummy callback
        struct DummyCb;
        impl solana_svm_callback::TransactionProcessingCallback for DummyCb {
            fn get_account_shared_data(&self, _pubkey: &Pubkey) -> Option<AccountSharedData> {
                None
            }
            fn account_matches_owners(
                &self,
                _account: &Pubkey,
                _owners: &[Pubkey],
            ) -> Option<usize> {
                None
            }
        }
        impl solana_svm_callback::InvokeContextCallback for DummyCb {}

        let output = vm.load_and_execute_sanitized_transactions(
            &DummyCb,
            &[tx],
            check_results,
            &env,
            &config,
        );

        // Should still produce a result (with empty accounts since program is unsupported)
        assert_eq!(output.processing_results.len(), 1);
        let result = output
            .processing_results
            .into_iter()
            .next()
            .unwrap()
            .unwrap();
        match result {
            ProcessedTransaction::Executed(executed) => {
                assert!(executed.loaded_transaction.accounts.is_empty());
            }
            _ => panic!("Expected Executed variant"),
        }
    }
}
