use pinocchio::{
    account_info::AccountInfo, entrypoint, program_error::ProgramError, pubkey::Pubkey,
    ProgramResult,
};

use crate::{
    processor::{
        process_add_operator, process_allow_mint, process_block_mint, process_create_instance,
        process_deposit, process_emit_event, process_release_funds, process_remove_operator,
        process_reset_smt_root, process_set_new_admin,
    },
    state::discriminator::ContraEscrowInstructionDiscriminators,
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

    let discriminator = ContraEscrowInstructionDiscriminators::try_from(*discriminator)
        .map_err(|_| ProgramError::InvalidInstructionData)?;

    match discriminator {
        ContraEscrowInstructionDiscriminators::CreateInstance => {
            process_create_instance(program_id, accounts, instruction_data)
        }
        ContraEscrowInstructionDiscriminators::AllowMint => {
            process_allow_mint(program_id, accounts, instruction_data)
        }
        ContraEscrowInstructionDiscriminators::BlockMint => {
            process_block_mint(program_id, accounts, instruction_data)
        }
        ContraEscrowInstructionDiscriminators::AddOperator => {
            process_add_operator(program_id, accounts, instruction_data)
        }
        ContraEscrowInstructionDiscriminators::RemoveOperator => {
            process_remove_operator(program_id, accounts, instruction_data)
        }
        ContraEscrowInstructionDiscriminators::SetNewAdmin => {
            process_set_new_admin(program_id, accounts, instruction_data)
        }
        ContraEscrowInstructionDiscriminators::Deposit => {
            process_deposit(program_id, accounts, instruction_data)
        }
        ContraEscrowInstructionDiscriminators::ReleaseFunds => {
            process_release_funds(program_id, accounts, instruction_data)
        }
        ContraEscrowInstructionDiscriminators::ResetSmtRoot => {
            process_reset_smt_root(program_id, accounts, instruction_data)
        }
        ContraEscrowInstructionDiscriminators::EmitEvent => {
            process_emit_event(program_id, accounts)
        }
    }
}
