use pinocchio::{
    account::AccountView,
    cpi::{invoke_signed, Seed, Signer},
    instruction::{InstructionAccount, InstructionView},
    Address, ProgramResult,
};

use crate::{
    constants::{event_authority_pda, EVENT_AUTHORITY_SEED},
    error::ContraEscrowProgramError,
};

/// Validates the event authority PDA and emits an event via CPI.
///
/// # Arguments
///
/// * `program_id` - The program ID
/// * `event_authority_info` - The event authority PDA account
/// * `event_data` - The serialized event data (should include EVENT_IX_TAG_LE prefix)
///
/// # Errors
///
/// Returns `ContraEscrowProgramError::InvalidEventAuthority` if the event authority PDA is invalid.
pub fn emit_event(
    program_id: &Address,
    event_authority_info: &AccountView,
    program_info: &AccountView,
    event_data: &[u8],
) -> ProgramResult {
    // Check that event authority PDA is valid.
    if event_authority_info.address().ne(&event_authority_pda::ID) {
        return Err(ContraEscrowProgramError::InvalidEventAuthority.into());
    }

    let signer_seeds = [
        Seed::from(EVENT_AUTHORITY_SEED),
        Seed::from(&[event_authority_pda::BUMP]),
    ];

    let signer = Signer::from(&signer_seeds);

    let accounts = [InstructionAccount::readonly_signer(
        event_authority_info.address(),
    )];

    // CPI to emit_event ix on same program to store event data in ix arg.
    invoke_signed(
        &InstructionView {
            program_id,
            accounts: &accounts,
            data: event_data,
        },
        &[event_authority_info, program_info],
        &[signer],
    )?;

    Ok(())
}
