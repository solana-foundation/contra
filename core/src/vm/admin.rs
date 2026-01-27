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

const SPL_TOKEN_ID: Pubkey = spl_token::id();

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
