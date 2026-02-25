extern crate alloc;

use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

use crate::{
    constants::event_authority_pda,
    error::ContraWithdrawProgramError,
    events::WithdrawFundsEvent,
    processor::{
        emit_event, validate_ata, verify_ata_program, verify_current_program, verify_mint_account,
        verify_signer, verify_token_program,
    },
    require_len,
};
use pinocchio_token::instructions::Burn;

/// Processes the WithdrawFunds instruction.
///
/// # Account Layout
/// 0. `[signer]` user - User initiating the withdrawal
/// 1. `[]` mint - Token mint
/// 2. `[writable]` token_account - Source token account
/// 3. `[]` token_program - Token program
/// 4. `[]` associated_token_program - Associated token program
/// 5. `[]` event_authority - Event authority PDA for emitting events
/// 6. `[]` contra_withdraw_program - Current program for CPI
///
/// # Instruction Data
/// * `amount` (u64) - Amount of tokens to withdraw
/// * `destination` (Pubkey) - Destination public key
pub fn process_withdraw_funds(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let args = process_instruction_data(instruction_data)?;

    if args.amount == 0 {
        return Err(ContraWithdrawProgramError::ZeroAmount.into());
    }

    let [user_info, mint_info, token_account_info, token_program_info, associated_token_program_info, event_authority_info, program_info] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    verify_signer(user_info, false)?;

    verify_ata_program(associated_token_program_info)?;
    verify_token_program(token_program_info)?;
    verify_current_program(program_info)?;

    if event_authority_info.key().ne(&event_authority_pda::ID) {
        return Err(ContraWithdrawProgramError::InvalidEventAuthority.into());
    }

    verify_mint_account(mint_info)?;

    validate_ata(
        token_account_info,
        user_info.key(),
        mint_info,
        token_program_info,
    )?;

    // SPL Burn of the token
    Burn {
        account: token_account_info,
        mint: mint_info,
        authority: user_info,
        amount: args.amount,
    }
    .invoke()?;

    let destination = args.destination.unwrap_or(*user_info.key());

    let withdraw_funds_event = WithdrawFundsEvent::new(args.amount, destination);
    let event_data = withdraw_funds_event.to_bytes();
    emit_event(program_id, event_authority_info, program_info, &event_data)?;

    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub struct WithdrawFundsArgs {
    pub amount: u64,
    pub destination: Option<Pubkey>,
}

fn process_instruction_data(instruction_data: &[u8]) -> Result<WithdrawFundsArgs, ProgramError> {
    require_len!(instruction_data, 9);

    let mut offset = 0;

    let amount = u64::from_le_bytes(
        instruction_data[offset..offset + 8]
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );

    offset += 8;

    let has_destination = instruction_data[offset] != 0;
    offset += 1;

    let destination = if has_destination {
        require_len!(instruction_data, offset + 32);

        let mut destination_bytes = [0u8; 32];
        destination_bytes.copy_from_slice(&instruction_data[offset..offset + 32]);
        Some(Pubkey::from(destination_bytes))
    } else {
        None
    };

    Ok(WithdrawFundsArgs {
        amount,
        destination,
    })
}
