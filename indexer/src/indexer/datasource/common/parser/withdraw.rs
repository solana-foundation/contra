use crate::{
    error::{account::AccountError, ParserError},
    indexer::datasource::common::types::CompiledInstruction,
    indexer::datasource::rpc_polling::types::InnerInstructions,
};
use borsh::BorshDeserialize;
use serde::{Deserialize, Serialize};
use solana_sdk::pubkey::Pubkey;

// Contra Withdraw Program ID
pub const CONTRA_WITHDRAW_PROGRAM_ID: &str = "J231K9UEpS4y4KAPwGc4gsMNCjKFRMYcQBcjVW7vBhVi";

// Instruction discriminators
const WITHDRAW_FUNDS: u8 = 0;

// Event related constants
const EVENT_IX_TAG_LE: &[u8] = &[0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d];
const WITHDRAW_FUNDS_EVENT_DISCRIMINATOR: u8 = 0;
const EVENT_DISCRIMINATOR_INDEX: usize = 8;
const EVENT_AMOUNT_START_INDEX: usize = 9;
const EVENT_DESTINATION_START_INDEX: usize = 17;
const WITHDRAW_FUNDS_EVENT_LEN: usize = 49;

// ******************************************************************************************
// Data types for instructions
// ******************************************************************************************
#[derive(BorshDeserialize)]
struct WithdrawFundsIxData {
    amount: u64,
    destination: Option<[u8; 32]>,
}

// ******************************************************************************************
// Instruction types
// ******************************************************************************************
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WithdrawInstruction {
    WithdrawFunds {
        accounts: WithdrawFundsAccounts,
        data: WithdrawFundsData,
        event: WithdrawFundsEventData,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WithdrawFundsAccounts {
    pub user: Pubkey,
    pub mint: Pubkey,
    pub token_account: Pubkey,
    pub token_program: Pubkey,
    pub associated_token_program: Pubkey,
    pub event_authority: Pubkey,
    pub contra_withdraw_program: Pubkey,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WithdrawFundsData {
    pub amount: u64,
    pub destination: Pubkey,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WithdrawFundsEventData {
    pub amount: u64,
    pub destination: Pubkey,
}

// ******************************************************************************************
// Parse instructions
// ******************************************************************************************
pub fn parse_withdraw_instruction(
    instruction: &CompiledInstruction,
    account_keys: &[Pubkey],
    inner_instructions: &[InnerInstructions],
) -> Result<Option<WithdrawInstruction>, ParserError> {
    // Decode base58 instruction data
    let data = bs58::decode(&instruction.data).into_vec()?;

    if data.is_empty() {
        return Ok(None);
    }

    let discriminator = data[0];
    let ix_data = &data[1..];

    match discriminator {
        WITHDRAW_FUNDS => {
            parse_withdraw_funds(ix_data, instruction, account_keys, inner_instructions)
        }
        _ => Ok(None), // Unsupported instruction type
    }
}

/// Parse WithdrawFunds instruction
fn parse_withdraw_funds(
    data: &[u8],
    instruction: &CompiledInstruction,
    account_keys: &[Pubkey],
    inner_instructions: &[InnerInstructions],
) -> Result<Option<WithdrawInstruction>, ParserError> {
    let ix_data = WithdrawFundsIxData::deserialize(&mut &data[..])?;

    // Expected 7 accounts
    if instruction.accounts.len() < 7 {
        return Err(AccountError::InsufficientAccounts {
            required: 7,
            actual: instruction.accounts.len(),
        }
        .into());
    }

    let user = account_keys[instruction.accounts[0] as usize];

    let accounts = WithdrawFundsAccounts {
        user,
        mint: account_keys[instruction.accounts[1] as usize],
        token_account: account_keys[instruction.accounts[2] as usize],
        token_program: account_keys[instruction.accounts[3] as usize],
        associated_token_program: account_keys[instruction.accounts[4] as usize],
        event_authority: account_keys[instruction.accounts[5] as usize],
        contra_withdraw_program: account_keys[instruction.accounts[6] as usize],
    };

    let instruction_destination = ix_data
        .destination
        .map(Pubkey::new_from_array)
        .unwrap_or(user);

    for inner_instruction_set in inner_instructions {
        for inner_instruction in &inner_instruction_set.instructions {
            let Ok(event_data) = bs58::decode(&inner_instruction.data).into_vec() else {
                continue;
            };

            if event_data.len() >= WITHDRAW_FUNDS_EVENT_LEN
                && event_data.starts_with(EVENT_IX_TAG_LE)
                && event_data[EVENT_DISCRIMINATOR_INDEX] == WITHDRAW_FUNDS_EVENT_DISCRIMINATOR
            {
                let amount = u64::from_le_bytes(
                    event_data[EVENT_AMOUNT_START_INDEX..EVENT_DESTINATION_START_INDEX]
                        .try_into()
                        .map_err(|_| ParserError::InstructionParseFailed {
                            reason: "Invalid withdraw event amount bytes".to_string(),
                        })?,
                );

                let event_destination = Pubkey::new_from_array(
                    event_data[EVENT_DESTINATION_START_INDEX..EVENT_DESTINATION_START_INDEX + 32]
                        .try_into()
                        .map_err(|_| ParserError::InstructionParseFailed {
                            reason: "Invalid withdraw event destination bytes".to_string(),
                        })?,
                );

                return Ok(Some(WithdrawInstruction::WithdrawFunds {
                    accounts,
                    data: WithdrawFundsData {
                        amount: ix_data.amount,
                        destination: instruction_destination,
                    },
                    event: WithdrawFundsEventData {
                        amount,
                        destination: event_destination,
                    },
                }));
            }
        }
    }

    Err(ParserError::InstructionParseFailed {
        reason: format!(
            "No withdraw funds event found for destination {}",
            instruction_destination
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============================================================================
    // Test Helper Functions
    // ============================================================================

    /// Create minimal valid Borsh-encoded data for WithdrawFunds instruction
    /// WithdrawFundsIxData { amount: u64, destination: Option<[u8; 32]> }
    fn create_withdraw_funds_borsh_data() -> Vec<u8> {
        let mut data = vec![];
        data.extend_from_slice(&1000u64.to_le_bytes()); // amount
        data.push(0); // None for destination (Option discriminator = 0)
        data
    }

    /// Create valid inner instruction data for WithdrawFunds event
    fn create_withdraw_funds_inner_instructions() -> Vec<InnerInstructions> {
        let mut data = vec![];

        data.extend_from_slice(EVENT_IX_TAG_LE);
        data.push(WITHDRAW_FUNDS_EVENT_DISCRIMINATOR);
        data.extend_from_slice(&1000u64.to_le_bytes());
        data.extend_from_slice(&[9u8; 32]);

        vec![InnerInstructions {
            index: 0,
            instructions: vec![CompiledInstruction {
                program_id_index: 0,
                accounts: vec![],
                data: bs58::encode(&data).into_string(),
            }],
        }]
    }

    /// Create N account keys for testing
    fn create_n_account_keys(n: usize) -> Vec<Pubkey> {
        (0..n)
            .map(|i| {
                let mut bytes = [0u8; 32];
                bytes[0] = i as u8;
                Pubkey::new_from_array(bytes)
            })
            .collect()
    }

    /// Create a CompiledInstruction with N accounts
    fn create_instruction_with_accounts(n_accounts: usize, data: String) -> CompiledInstruction {
        CompiledInstruction {
            program_id_index: 0,
            accounts: (0..n_accounts as u8).collect(),
            data,
        }
    }

    // ============================================================================
    // parse_withdraw_funds Tests
    // ============================================================================

    #[test]
    fn test_withdraw_funds_valid_accounts() {
        let borsh_data = create_withdraw_funds_borsh_data();
        let instruction = create_instruction_with_accounts(7, "dummy".to_string());
        let account_keys = create_n_account_keys(7);

        let result = parse_withdraw_funds(
            &borsh_data,
            &instruction,
            &account_keys,
            &create_withdraw_funds_inner_instructions(),
        );

        assert!(result.is_ok());
        let parsed = result.unwrap();
        assert!(parsed.is_some());

        if let Some(WithdrawInstruction::WithdrawFunds { data, event, .. }) = parsed {
            assert_eq!(data.amount, 1000);
            assert_eq!(event.amount, 1000);
            assert_eq!(event.destination, Pubkey::new_from_array([9u8; 32]));
        } else {
            panic!("Expected WithdrawFunds instruction");
        }
    }

    #[test]
    fn test_withdraw_funds_event_not_found() {
        let borsh_data = create_withdraw_funds_borsh_data();
        let instruction = create_instruction_with_accounts(7, "dummy".to_string());
        let account_keys = create_n_account_keys(7);

        let result = parse_withdraw_funds(&borsh_data, &instruction, &account_keys, &[]);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("No withdraw funds event found"),
            "Error: {}",
            err
        );
    }

    #[test]
    fn test_withdraw_funds_insufficient_accounts() {
        let borsh_data = create_withdraw_funds_borsh_data();
        let instruction = create_instruction_with_accounts(6, "dummy".to_string()); // Only 6 accounts (need 7)
        let account_keys = create_n_account_keys(6);

        let result = parse_withdraw_funds(
            &borsh_data,
            &instruction,
            &account_keys,
            &create_withdraw_funds_inner_instructions(),
        );

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Insufficient accounts"), "Error: {}", err);
    }
}
