use pinocchio::{
    account::AccountView,
    cpi::{invoke_signed, Seed, Signer},
    instruction::{InstructionAccount, InstructionView},
    Address, ProgramResult,
};

use crate::{
    constants::{event_authority_pda, EVENT_AUTHORITY_SEED},
    error::PrivateChannelEscrowProgramError,
};

pub fn emit_event(
    program_id: &Address,
    event_authority_info: &AccountView,
    program_info: &AccountView,
    event_data: &[u8],
) -> ProgramResult {
    if event_authority_info.address().ne(&event_authority_pda::ID) {
        return Err(PrivateChannelEscrowProgramError::InvalidEventAuthority.into());
    }

    let signer_seeds = [
        Seed::from(EVENT_AUTHORITY_SEED),
        Seed::from(&[event_authority_pda::BUMP]),
    ];

    let signer = Signer::from(&signer_seeds);

    let accounts = [InstructionAccount::readonly_signer(
        event_authority_info.address(),
    )];

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
