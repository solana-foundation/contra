extern crate alloc;

use crate::{
    error::PrivateChannelEscrowProgramError,
    events::BlockMintEvent,
    processor::{
        shared::{
            account_check::{verify_signer, verify_system_program},
            event_utils::emit_event,
        },
        verify_current_program,
    },
    state::{AllowedMint, Instance},
    validate_event_authority,
};
use pinocchio::{account::AccountView, error::ProgramError, Address, ProgramResult};

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
/// 7. `[]` private_channel_escrow_program - Current program for CPI
///
/// # Instruction Data
/// * None - No instruction data required
pub fn process_block_mint(
    program_id: &Address,
    accounts: &[AccountView],
    _instruction_data: &[u8],
) -> ProgramResult {
    let [payer_info, admin_info, instance_info, mint_info, allowed_mint_info, system_program_info, event_authority_info, program_info] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    verify_signer(payer_info, true)?;
    verify_signer(admin_info, false)?;
    verify_system_program(system_program_info)?;
    verify_current_program(program_info)?;

    validate_event_authority!(event_authority_info);

    let instance_data = instance_info.try_borrow()?;
    let instance = Instance::try_from_bytes(&instance_data)?;

    instance
        .validate_pda(instance_info)
        .map_err(|_| PrivateChannelEscrowProgramError::InvalidInstance)?;

    instance.validate_admin(admin_info.address())?;

    let allowed_mint_data = allowed_mint_info.try_borrow()?;
    let allowed_mint = AllowedMint::try_from_bytes(&allowed_mint_data)?;

    allowed_mint
        .validate_pda(
            instance_info.address(),
            mint_info.address(),
            allowed_mint_info,
        )
        .map_err(|_| PrivateChannelEscrowProgramError::InvalidAllowedMint)?;

    drop(allowed_mint_data);

    let payer_lamports = payer_info.lamports();
    payer_info.set_lamports(
        payer_lamports
            .checked_add(allowed_mint_info.lamports())
            .unwrap(),
    );
    allowed_mint_info.set_lamports(0);
    allowed_mint_info.close()?;

    let event = BlockMintEvent::new(instance.instance_seed, *mint_info.address());
    emit_event(
        program_id,
        event_authority_info,
        program_info,
        &event.to_bytes(),
    )?;

    Ok(())
}
