use pinocchio::{
    account_info::AccountInfo,
    instruction::{AccountMeta, Instruction, Seed, Signer},
    program::invoke_signed,
    pubkey::Pubkey,
    ProgramResult,
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
    program_id: &Pubkey,
    event_authority_info: &AccountInfo,
    program_info: &AccountInfo,
    event_data: &[u8],
) -> ProgramResult {
    // Check that event authority PDA is valid.
    if event_authority_info.key().ne(&event_authority_pda::ID) {
        return Err(ContraEscrowProgramError::InvalidEventAuthority.into());
    }

    let signer_seeds = [
        Seed::from(EVENT_AUTHORITY_SEED),
        Seed::from(&[event_authority_pda::BUMP]),
    ];

    let signer = Signer::from(&signer_seeds);

    // CPI to emit_event ix on same program to store event data in ix arg.
    invoke_signed(
        &Instruction {
            program_id,
            accounts: &[AccountMeta::new(event_authority_info.key(), false, true)],
            data: event_data,
        },
        &[event_authority_info, program_info],
        &[signer],
    )?;

    Ok(())
}
