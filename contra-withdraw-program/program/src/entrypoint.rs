use pinocchio::{account::AccountView, entrypoint, error::ProgramError, Address, ProgramResult};

use crate::{
    discriminator::ContraWithdrawInstructionDiscriminators, processor::process_withdraw_funds,
};

entrypoint!(process_instruction);

pub fn process_instruction(
    program_id: &Address,
    accounts: &[AccountView],
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
    }
}
