use crate::error::ContraWithdrawProgramError;
use pinocchio::{account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey};
use pinocchio_associated_token_account::ID as ATA_PROGRAM_ID;
use pinocchio_token::{state::Mint, ID as TOKEN_PROGRAM_ID};

/// Verify account as a signer, returning an error if it is not or if it is not writable while
/// expected to be.
///
/// # Arguments
/// * `info` - The account to verify.
/// * `expect_writable` - Whether the account should be writable
///
/// # Returns
/// * `Result<(), ProgramError>` - The result of the operation
#[inline(always)]
pub fn verify_signer(info: &AccountInfo, expect_writable: bool) -> Result<(), ProgramError> {
    if !info.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if expect_writable && !info.is_writable() {
        return Err(ProgramError::InvalidAccountData);
    }

    Ok(())
}

/// Verify account's owner and account mutability.
///
/// # Arguments
/// * `info` - The account to verify.
/// * `owner` - The expected owner of the account.
/// * `expect_writable` - Whether the account is expected to be writable.
///
/// # Returns
/// * `Result<(), ProgramError>` - The result of the operation
#[inline(always)]
pub fn verify_owner_mutability(
    info: &AccountInfo,
    owner: &Pubkey,
    expect_writable: bool,
) -> Result<(), ProgramError> {
    if !info.is_owned_by(owner) {
        return Err(ProgramError::InvalidAccountOwner);
    }
    if expect_writable && !info.is_writable() {
        return Err(ProgramError::InvalidAccountData);
    }

    Ok(())
}

/// Verify account as Associated Token program, returning an error if it is not.
///
/// # Arguments
/// * `info` - The account to verify.
///
/// # Returns
/// * `Result<(), ProgramError>` - The result of the operation
#[inline(always)]
pub fn verify_ata_program(info: &AccountInfo) -> Result<(), ProgramError> {
    if info.key().ne(&ATA_PROGRAM_ID) {
        return Err(ProgramError::IncorrectProgramId);
    }

    Ok(())
}

/// Verify account as Tokenkeg program, returning an error if it is not.
///
/// # Arguments
/// * `info` - The account to verify.
///
/// # Returns
/// * `Result<(), ProgramError>` - The result of the operation
#[inline(always)]
pub fn verify_token_program(info: &AccountInfo) -> Result<(), ProgramError> {
    if info.key().ne(&TOKEN_PROGRAM_ID) {
        return Err(ProgramError::IncorrectProgramId);
    }

    Ok(())
}

#[inline(always)]
pub fn verify_token_program_account(info: &AccountInfo) -> Result<(), ProgramError> {
    if !info.is_owned_by(&TOKEN_PROGRAM_ID) {
        return Err(ProgramError::InvalidAccountOwner);
    }

    Ok(())
}

/// Verify account as a valid Mint account
#[inline(always)]
pub fn verify_mint_account(info: &AccountInfo) -> Result<(), ProgramError> {
    let _ = Mint::from_account_info(info).map_err(|_| ContraWithdrawProgramError::InvalidMint)?;

    Ok(())
}
