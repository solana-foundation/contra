use pinocchio::{
    account_info::AccountInfo,
    instruction::{AccountMeta, Instruction, Seed, Signer},
    program::invoke_signed,
    pubkey::Pubkey,
    ProgramResult,
};

use crate::{
    constants::{event_authority_pda, EVENT_AUTHORITY_SEED},
    error::ContraWithdrawProgramError,
};

pub fn emit_event(
    program_id: &Pubkey,
    event_authority_info: &AccountInfo,
    program_info: &AccountInfo,
    event_data: &[u8],
) -> ProgramResult {
    if event_authority_info.key().ne(&event_authority_pda::ID) {
        return Err(ContraWithdrawProgramError::InvalidEventAuthority.into());
    }

    let signer_seeds = [
        Seed::from(EVENT_AUTHORITY_SEED),
        Seed::from(&[event_authority_pda::BUMP]),
    ];

    let signer = Signer::from(&signer_seeds);

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
