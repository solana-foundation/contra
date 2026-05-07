use pinocchio::{account::AccountView, address::Address, error::ProgramError, ProgramResult};

/// Validates that an Associated Token Account address matches the expected
/// derivation for the given wallet and mint, and that the account has data.
#[inline(always)]
pub fn validate_ata(
    ata_info: &AccountView,
    wallet_key: &Address,
    mint_info: &AccountView,
    token_program_info: &AccountView,
) -> ProgramResult {
    let expected_ata = Address::find_program_address(
        &[
            wallet_key.as_ref(),
            token_program_info.address().as_ref(),
            mint_info.address().as_ref(),
        ],
        &pinocchio_associated_token_account::ID,
    )
    .0;

    if ata_info.address() != &expected_ata || ata_info.is_data_empty() {
        return Err(ProgramError::InvalidInstructionData);
    }

    Ok(())
}
