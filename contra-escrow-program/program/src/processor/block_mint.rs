extern crate alloc;

use crate::{
    error::ContraEscrowProgramError,
    events::BlockMintEvent,
    processor::{
        shared::{
            account_check::{verify_signer, verify_system_program},
            event_utils::emit_event,
        },
        verify_current_program,
    },
    state::{AllowedMint, Instance},
    validate_event_accounts,
};
use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

/// Processes the BlockMint instruction.
///
/// # Account Layout
/// 0. `[signer, writable]` payer - Receives the rent reclaimed from closed account
/// 1. `[signer]` admin - Admin of the instance
/// 2. `[]` instance - Instance PDA to validate admin authority
/// 3. `[]` mint - Token mint to be blocked
/// 4. `[writable]` allowed_mint - AllowedMint PDA to be closed
/// 5. `[]` system_program - System program (not used but kept for consistency)
/// 6. `[signer]` event_authority - Event authority PDA for emitting events
/// 7. `[]` contra_escrow_program - Current program for CPI
///
/// # Instruction Data
/// * None - No instruction data required
pub fn process_block_mint(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    _instruction_data: &[u8],
) -> ProgramResult {
    let [payer_info, admin_info, instance_info, mint_info, allowed_mint_info, system_program_info, event_authority_info, program_info] =
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
    let instance_data = instance_info.try_borrow_data()?;
    let instance = Instance::try_from_bytes(&instance_data)?;

    instance
        .validate_pda(instance_info)
        .map_err(|_| ContraEscrowProgramError::InvalidInstance)?;

    instance.validate_admin(admin_info.key())?;

    // Validate allowed mint account exists and is correct PDA
    let allowed_mint_data = allowed_mint_info.try_borrow_data()?;
    let allowed_mint = AllowedMint::try_from_bytes(&allowed_mint_data)?;

    allowed_mint
        .validate_pda(instance_info.key(), mint_info.key(), allowed_mint_info)
        .map_err(|_| ContraEscrowProgramError::InvalidAllowedMint)?;

    // Close the AllowedMint account
    drop(allowed_mint_data);

    let payer_lamports = payer_info.lamports();
    *payer_info.try_borrow_mut_lamports().unwrap() = payer_lamports
        .checked_add(allowed_mint_info.lamports())
        .unwrap();
    *allowed_mint_info.try_borrow_mut_lamports().unwrap() = 0;
    allowed_mint_info.close()?;

    // Emit BlockMint event
    let event = BlockMintEvent::new(instance.instance_seed, *mint_info.key());
    emit_event(
        program_id,
        event_authority_info,
        program_info,
        &event.to_bytes(),
    )?;

    Ok(())
}
