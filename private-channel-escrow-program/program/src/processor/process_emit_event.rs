use pinocchio::{account::AccountView, error::ProgramError, Address, ProgramResult};

use crate::{
    constants::event_authority_pda, error::PrivateChannelEscrowProgramError, processor::verify_signer,
};

#[inline(always)]
pub fn process_emit_event(_program_id: &Address, accounts: &[AccountView]) -> ProgramResult {
    let [event_authority] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if event_authority.address().ne(&event_authority_pda::ID) {
        return Err(PrivateChannelEscrowProgramError::InvalidEventAuthority.into());
    }

    verify_signer(event_authority, false)?;

    Ok(())
}
