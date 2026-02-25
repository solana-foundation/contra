use pinocchio::{
    account_info::AccountInfo, entrypoint, program_error::ProgramError, pubkey::Pubkey,
    ProgramResult,
};

use crate::{
    discriminator::ContraWithdrawInstructionDiscriminators,
    processor::{process_emit_event, process_withdraw_funds},
};

entrypoint!(process_instruction);

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let (discriminator, instruction_data) = instruction_data
        .split_first()
        .ok_or(ProgramError::InvalidInstructionData)?;

    let discriminator = ContraWithdrawInstructionDiscriminators::try_from(*discriminator)
        .map_err(|_| ProgramError::InvalidInstructionData)?;

    match discriminator {
        ContraWithdrawInstructionDiscriminators::WithdrawFunds => {
            process_withdraw_funds(program_id, accounts, instruction_data)
        }
        ContraWithdrawInstructionDiscriminators::EmitEvent => {
            process_emit_event(program_id, accounts)
        }
    }
}
