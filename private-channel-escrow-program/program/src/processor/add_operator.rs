extern crate alloc;

use crate::{
    constants::OPERATOR_SEED,
    error::PrivateChannelEscrowProgramError,
    events::AddOperatorEvent,
    processor::{
        shared::{
            account_check::{verify_signer, verify_system_account, verify_system_program},
            event_utils::emit_event,
            pda_utils::create_pda_account,
        },
        verify_current_program,
    },
    require_len,
    state::{discriminator::AccountSerialize, Instance, Operator},
    validate_event_authority,
};
use pinocchio::{
    account::AccountView,
    cpi::Seed,
    error::ProgramError,
    sysvars::{rent::Rent, Sysvar},
    Address, ProgramResult,
};

/// Processes the AddOperator instruction.
///
/// # Account Layout
/// 0. `[signer, writable]` payer - Pays for the account creation
/// 1. `[signer]` admin - Admin of the instance
/// 2. `[]` instance - Instance PDA to validate admin authority
/// 3. `[]` operator - Wallet to be granted operator access
/// 4. `[writable]` operator_pda - Operator PDA to be created
/// 5. `[]` system_program - System program for account creation
/// 6. `[signer]` event_authority - Event authority PDA for emitting events
/// 7. `[]` private_channel_escrow_program - Current program for CPI
///
/// # Instruction Data
/// * `bump` (u8) - Bump for the operator PDA
pub fn process_add_operator(
    program_id: &Address,
    accounts: &[AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let args = process_instruction_data(instruction_data)?;
    let [payer_info, admin_info, instance_info, operator_pda, operator_pda_info, system_program_info, event_authority_info, program_info] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    verify_signer(payer_info, true)?;
    verify_signer(admin_info, false)?;
    verify_system_account(operator_pda_info, true)?;
    verify_system_program(system_program_info)?;
    verify_current_program(program_info)?;

    validate_event_authority!(event_authority_info);

    let instance_data = instance_info.try_borrow()?;
    let instance = Instance::try_from_bytes(&instance_data)?;

    instance
        .validate_pda(instance_info)
        .map_err(|_| PrivateChannelEscrowProgramError::InvalidInstance)?;

    instance.validate_admin(admin_info.address())?;

    let operator = Operator::new(args.bump);
    operator.validate_pda(
        instance_info.address(),
        operator_pda.address(),
        operator_pda_info,
    )?;

    let bump_seed = [args.bump];
    let operator_seeds = [
        Seed::from(OPERATOR_SEED),
        Seed::from(instance_info.address().as_ref()),
        Seed::from(operator_pda.address().as_ref()),
        Seed::from(&bump_seed),
    ];

    let rent = Rent::get()?;
    create_pda_account(
        payer_info,
        &rent,
        Operator::LEN,
        program_id,
        operator_pda_info,
        operator_seeds,
        None,
    )?;

    let operator_data = operator.to_bytes();
    let mut data_slice = operator_pda_info.try_borrow_mut()?;
    data_slice[..operator_data.len()].copy_from_slice(&operator_data);

    let event = AddOperatorEvent::new(instance.instance_seed, *operator_pda.address());
    emit_event(
        program_id,
        event_authority_info,
        program_info,
        &event.to_bytes(),
    )?;

    Ok(())
}

struct AddOperatorArgs {
    bump: u8,
}

fn process_instruction_data(data: &[u8]) -> Result<AddOperatorArgs, ProgramError> {
    require_len!(data, 1);
    Ok(AddOperatorArgs { bump: data[0] })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ID as PRIVATE_CHANNEL_ESCROW_PROGRAM_ID;
    use alloc::vec;

    #[test]
    fn test_process_add_operator_valid_bump() {
        // Test with valid bump
        let instruction_data = vec![123]; // bump = 123

        let result = process_instruction_data(&instruction_data);

        assert!(result.is_ok());
        assert_eq!(result.unwrap().bump, 123);
    }

    #[test]
    fn test_process_add_operator_empty_instruction_data() {
        let instruction_data = [];
        let accounts = [];

        let result = process_add_operator(&PRIVATE_CHANNEL_ESCROW_PROGRAM_ID, &accounts, &instruction_data);

        assert_eq!(result.unwrap_err(), ProgramError::InvalidInstructionData);
    }
}
