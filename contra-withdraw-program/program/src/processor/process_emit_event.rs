use pinocchio::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult,
};

use crate::{
    constants::event_authority_pda, error::ContraWithdrawProgramError, processor::verify_signer,
};

pub fn process_emit_event(_program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let [event_authority] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if event_authority.key().ne(&event_authority_pda::ID) {
        return Err(ContraWithdrawProgramError::InvalidEventAuthority.into());
    }

    verify_signer(event_authority, false)?;

    Ok(())
}
