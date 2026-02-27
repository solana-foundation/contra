extern crate alloc;

use crate::{
    error::ContraEscrowProgramError,
    events::SetNewAdminEvent,
    processor::{
        shared::{account_check::verify_signer, event_utils::emit_event},
        verify_current_program,
    },
    state::{discriminator::AccountSerialize, Instance},
    validate_event_accounts,
};
use pinocchio::{account::AccountView, error::ProgramError, Address, ProgramResult};

/// Processes the SetNewAdmin instruction.
///
/// # Account Layout
/// 0. `[signer, writable]` payer - Transaction fee payer
/// 1. `[signer]` current_admin - Current admin of the instance
/// 2. `[writable]` instance - Instance PDA to be updated
/// 3. `[signer]` new_admin - New admin to be set (must sign)
/// 4. `[signer]` event_authority - Event authority PDA for emitting events
/// 5. `[]` contra_escrow_program - Current program for CPI
///
/// # Instruction Data
/// * None - No instruction data required
pub fn process_set_new_admin(
    program_id: &Address,
    accounts: &[AccountView],
    _instruction_data: &[u8],
) -> ProgramResult {
    let [payer_info, current_admin_info, instance_info, new_admin_info, event_authority_info, program_info] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    // Validate account signatures and mutability
    verify_signer(payer_info, true)?;
    verify_signer(current_admin_info, false)?;
    verify_signer(new_admin_info, false)?;

    // Verify programs
    verify_current_program(program_info)?;

    validate_event_accounts!(event_authority_info, program_info);

    // Validate instance exists and current admin has authority
    let instance_data = instance_info.try_borrow()?;
    let mut instance = Instance::try_from_bytes(&instance_data)?;

    instance
        .validate_pda(instance_info)
        .map_err(|_| ContraEscrowProgramError::InvalidInstance)?;

    instance.validate_admin(current_admin_info.address())?;

    // Store the old admin for the event
    let old_admin = instance.admin;

    // Update the instance with the new admin
    instance.admin = *new_admin_info.address();

    // Serialize updated instance and write back to account
    let updated_instance_data = instance.to_bytes();

    drop(instance_data);

    let mut data_slice = instance_info.try_borrow_mut()?;
    data_slice[..updated_instance_data.len()].copy_from_slice(&updated_instance_data);

    // Emit SetNewAdmin event
    let event = SetNewAdminEvent::new(instance.instance_seed, old_admin, instance.admin);
    emit_event(
        program_id,
        event_authority_info,
        program_info,
        &event.to_bytes(),
    )?;

    Ok(())
}
