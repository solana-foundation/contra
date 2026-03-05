use pinocchio::{account::AccountView, error::ProgramError, Address, ProgramResult};
use pinocchio_token::instructions::Burn;

use crate::{
    error::ContraWithdrawProgramError,
    events::WithdrawFundsEvent,
    processor::{
        validate_ata, verify_ata_program, verify_mint_account, verify_signer, verify_token_program,
    },
    require_len,
};

/// Processes the WithdrawFunds instruction.
///
/// # Account Layout
/// 0. `[signer]` user - User initiating the withdrawal
/// 1. `[]` mint - Token mint
/// 2. `[writable]` token_account - Source token account
/// 3. `[]` token_program - Token program
/// 4. `[]` associated_token_program - Associated token program
///
/// # Instruction Data
/// * `amount` (u64) - Amount of tokens to withdraw
/// * `destination` (Option<Pubkey>) - Destination public key
pub fn process_withdraw_funds(
    _program_id: &Address,
    accounts: &[AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let args = parse_instruction_data(instruction_data)?;

    if args.amount == 0 {
        return Err(ContraWithdrawProgramError::ZeroAmount.into());
    }

    let [user_info, mint_info, token_account_info, token_program_info, associated_token_program_info] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    verify_signer(user_info, false)?;
    verify_ata_program(associated_token_program_info)?;
    verify_token_program(token_program_info)?;
    verify_mint_account(mint_info)?;

    validate_ata(
        token_account_info,
        user_info.address(),
        mint_info,
        token_program_info,
    )?;

    Burn {
        account: token_account_info,
        mint: mint_info,
        authority: user_info,
        amount: args.amount,
    }
    .invoke()?;

    let event = WithdrawFundsEvent {
        amount: args.amount,
        destination: args.destination.unwrap_or(*user_info.address()),
    };
    pinocchio_log::log!("{}", event.to_bytes().as_slice());

    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub struct WithdrawFundsArgs {
    pub amount: u64,
    pub destination: Option<Address>,
}

fn parse_instruction_data(data: &[u8]) -> Result<WithdrawFundsArgs, ProgramError> {
    require_len!(data, 9);

    let amount = u64::from_le_bytes(
        data[..8]
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );

    let destination = if data[8] != 0 {
        require_len!(data, 9 + 32);
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&data[9..41]);
        Some(Address::new_from_array(bytes))
    } else {
        None
    };

    Ok(WithdrawFundsArgs {
        amount,
        destination,
    })
}
