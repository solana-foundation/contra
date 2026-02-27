extern crate alloc;

use crate::{
    error::ContraEscrowProgramError,
    events::RemoveOperatorEvent,
    processor::{
        shared::{
            account_check::{verify_signer, verify_system_program},
            event_utils::emit_event,
        },
        verify_current_program,
    },
    state::{Instance, Operator},
    validate_event_accounts,
};
use pinocchio::{account::AccountView, error::ProgramError, Address, ProgramResult};

/// Processes the RemoveOperator instruction.
///
/// # Account Layout
/// 0. `[signer, writable]` payer - Receives the rent reclaimed from closed account
/// 1. `[signer]` admin - Admin of the instance
/// 2. `[]` instance - Instance PDA to validate admin authority
/// 3. `[]` operator - Wallet that had operator access
/// 4. `[writable]` operator_pda - Operator PDA to be closed
/// 5. `[]` system_program - System program (not used but kept for consistency)
/// 6. `[signer]` event_authority - Event authority PDA for emitting events
/// 7. `[]` contra_escrow_program - Current program for CPI
///
/// # Instruction Data
/// * None - No instruction data required
pub fn process_remove_operator(
    program_id: &Address,
    accounts: &[AccountView],
    _instruction_data: &[u8],
) -> ProgramResult {
    let [payer_info, admin_info, instance_info, operator_info, operator_pda_info, system_program_info, event_authority_info, program_info] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    // Validate account signatures and mutability
    verify_signer(payer_info, true)?;
    verify_signer(admin_info, false)?;

    // Verify programs
    verify_system_program(system_program_info)?;
    verify_current_program(program_info)?;

    validate_event_accounts!(event_authority_info, program_info);

    // Validate instance exists and admin has authority
    let instance_data = instance_info.try_borrow()?;
    let instance = Instance::try_from_bytes(&instance_data)?;

    instance
        .validate_pda(instance_info)
        .map_err(|_| ContraEscrowProgramError::InvalidInstance)?;

    instance.validate_admin(admin_info.address())?;

    // Validate operator account exists and is correct PDA
    let operator_data = operator_pda_info.try_borrow()?;
    let operator = Operator::try_from_bytes(&operator_data)?;

    operator.validate_pda(
        instance_info.address(),
        operator_info.address(),
        operator_pda_info,
    )?;

    // Close the Operator account
    drop(operator_data);

    let payer_lamports = payer_info.lamports();
    payer_info.set_lamports(
        payer_lamports
            .checked_add(operator_pda_info.lamports())
            .unwrap(),
    );
    operator_pda_info.set_lamports(0);
    operator_pda_info.close()?;

    // Emit RemoveOperator event
    let event = RemoveOperatorEvent::new(instance.instance_seed, *operator_info.address());
    emit_event(
        program_id,
        event_authority_info,
        program_info,
        &event.to_bytes(),
    )?;

    Ok(())
}
