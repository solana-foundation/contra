use pinocchio::{account::AccountView, address::Address, error::ProgramError, ProgramResult};

/// Validates an Associated Token Account address.
///
/// # Arguments
/// * `ata_info` - The ATA account to validate/create
/// * `wallet_key` - The wallet that should own the ATA
/// * `mint_info` - The token mint for the ATA
/// * `token_program_info` - The token program account
///
/// # Returns
/// * `ProgramResult` - Success if validation passes and ATA exists
#[inline(always)]
pub fn validate_ata(
    ata_info: &AccountView,
    wallet_key: &Address,
    mint_info: &AccountView,
    token_program_info: &AccountView,
) -> ProgramResult {
    // Validate ATA address is correct for this wallet + mint
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
