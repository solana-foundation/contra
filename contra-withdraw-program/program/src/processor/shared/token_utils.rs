use pinocchio::{
    account_info::AccountInfo,
    program_error::ProgramError,
    pubkey::{find_program_address, Pubkey},
    ProgramResult,
};

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
    ata_info: &AccountInfo,
    wallet_key: &Pubkey,
    mint_info: &AccountInfo,
    token_program_info: &AccountInfo,
) -> ProgramResult {
    // Validate ATA address is correct for this wallet + mint
    let expected_ata = find_program_address(
        &[
            wallet_key.as_ref(),
            token_program_info.key().as_ref(),
            mint_info.key().as_ref(),
        ],
        &pinocchio_associated_token_account::ID,
    )
    .0;

    if ata_info.key() != &expected_ata || ata_info.data_is_empty() {
        return Err(ProgramError::InvalidInstructionData);
    }

    Ok(())
}
