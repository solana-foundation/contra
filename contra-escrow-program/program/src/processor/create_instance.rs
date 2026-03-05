extern crate alloc;

use crate::{
    constants::INSTANCE_SEED,
    events::CreateInstanceEvent,
    processor::{
        shared::{
            account_check::{verify_signer, verify_system_account, verify_system_program},
            event_utils::emit_event,
            pda_utils::create_pda_account,
        },
        verify_current_program,
    },
    require_len,
    state::{discriminator::AccountSerialize, Instance},
    validate_event_authority,
};
use pinocchio::{
    account::AccountView,
    cpi::Seed,
    error::ProgramError,
    sysvars::{rent::Rent, Sysvar},
    Address, ProgramResult,
};

/// Processes the CreateInstance instruction.
///
/// # Account Layout
/// 0. `[signer, writable]` payer - Pays for the account creation
/// 1. `[signer]` admin - Admin of the instance
/// 2. `[signer]` instance_seed - Instance seed signer for PDA derivation
/// 3. `[writable]` instance - Instance PDA to be created
/// 4. `[]` system_program - System program for account creation
/// 5. `[signer]` event_authority - Event authority PDA for emitting events
/// 6. `[]` contra_escrow_program - Current program for CPI
///
/// # Instruction Data
/// * `bump` (u8) - Bump for the instance PDA
pub fn process_create_instance(
    program_id: &Address,
    accounts: &[AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let args = process_instruction_data(instruction_data)?;
    let [payer_info, admin_info, instance_seed_info, instance_info, system_program_info, event_authority_info, program_info] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    verify_signer(payer_info, true)?;
    verify_signer(admin_info, false)?;
    verify_signer(instance_seed_info, false)?;
    verify_system_account(instance_info, true)?;
    verify_system_program(system_program_info)?;
    verify_current_program(program_info)?;

    validate_event_authority!(event_authority_info);

    let instance = Instance::new(
        args.bump,
        *instance_seed_info.address(),
        *admin_info.address(),
    );
    instance.validate_pda(instance_info)?;

    let bump_seed = [args.bump];
    let instance_seeds = [
        Seed::from(INSTANCE_SEED),
        Seed::from(instance.instance_seed.as_ref()),
        Seed::from(&bump_seed),
    ];

    let rent = Rent::get()?;
    create_pda_account(
        payer_info,
        &rent,
        Instance::LEN,
        program_id,
        instance_info,
        instance_seeds,
        None,
    )?;

    let instance_data = instance.to_bytes();
    let mut data_slice = instance_info.try_borrow_mut()?;
    data_slice[..instance_data.len()].copy_from_slice(&instance_data);

    let event = CreateInstanceEvent::new(*instance_seed_info.address(), *admin_info.address());
    emit_event(
        program_id,
        event_authority_info,
        program_info,
        &event.to_bytes(),
    )?;

    Ok(())
}

struct CreateInstanceArgs {
    bump: u8,
}

fn process_instruction_data(data: &[u8]) -> Result<CreateInstanceArgs, ProgramError> {
    require_len!(data, 1);
    Ok(CreateInstanceArgs { bump: data[0] })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ID as CONTRA_ESCROW_PROGRAM_ID;

    #[test]
    fn test_process_instruction_data_valid() {
        let instruction_data = [1]; // bump only

        let result = process_instruction_data(&instruction_data);
        assert!(result.is_ok());
        let args = result.unwrap();
        assert_eq!(args.bump, 1);
    }

    #[test]
    fn test_process_instruction_data_insufficient_data() {
        let instruction_data = []; // No data
        let result = process_instruction_data(&instruction_data);
        assert_eq!(result.err(), Some(ProgramError::InvalidInstructionData));
    }

    #[test]
    fn test_process_create_instance_empty_instruction_data() {
        let instruction_data = [];
        let accounts = [];

        let result =
            process_create_instance(&CONTRA_ESCROW_PROGRAM_ID, &accounts, &instruction_data);

        // Empty data triggers InvalidInstructionData
        assert_eq!(result.unwrap_err(), ProgramError::InvalidInstructionData);
    }
}
