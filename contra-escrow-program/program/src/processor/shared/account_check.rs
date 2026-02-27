use crate::ID as CONTRA_ESCROW_PROGRAM_ID;
use pinocchio::{account::AccountView, address::Address, error::ProgramError};
use pinocchio_associated_token_account::ID as ATA_PROGRAM_ID;
use pinocchio_token::ID as TOKEN_PROGRAM_ID;
use pinocchio_token_2022::ID as TOKEN_2022_PROGRAM_ID;

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
pub fn verify_signer(info: &AccountView, expect_writable: bool) -> Result<(), ProgramError> {
    if !info.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if expect_writable && !info.is_writable() {
        return Err(ProgramError::InvalidAccountData);
    }

    Ok(())
}

#[inline(always)]
pub fn verify_account_owner(
    info: &AccountView,
    expected_owner: &Address,
) -> Result<(), ProgramError> {
    if !info.owned_by(expected_owner) {
        return Err(ProgramError::InvalidAccountOwner);
    }

    Ok(())
}

/// Verify account's mutability.
///
/// # Arguments
/// * `info` - The account to verify.
/// * `is_writable` - Whether the account is expected to be writable.
///
/// # Returns
/// * `Result<(), ProgramError>` - The result of the operation
#[inline(always)]
pub fn verify_mutability(info: &AccountView, expect_writable: bool) -> Result<(), ProgramError> {
    if expect_writable && !info.is_writable() {
        return Err(ProgramError::InvalidAccountData);
    }

    Ok(())
}

/// Verify account as a system account, returning an error if it is not or if it is not writable
/// while expected to be.
///
/// # Arguments
/// * `info` - The account to verify.
/// * `is_writable` - Whether the account should be writable.
///
/// # Returns
/// * `Result<(), ProgramError>` - The result of the operation
#[inline(always)]
pub fn verify_system_account(info: &AccountView, is_writable: bool) -> Result<(), ProgramError> {
    if !info.owned_by(&pinocchio_system::ID) {
        return Err(ProgramError::InvalidAccountOwner);
    }

    if !info.is_data_empty() {
        return Err(ProgramError::AccountAlreadyInitialized);
    }

    if is_writable && !info.is_writable() {
        return Err(ProgramError::InvalidAccountData);
    }

    Ok(())
}

/// Verify account as system program, returning an error if it is not.
///
/// # Arguments
/// * `info` - The account to verify.
///
/// # Returns
/// * `Result<(), ProgramError>` - The result of the operation
#[inline(always)]
pub fn verify_system_program(info: &AccountView) -> Result<(), ProgramError> {
    if info.address().ne(&pinocchio_system::ID) {
        return Err(ProgramError::IncorrectProgramId);
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
pub fn verify_ata_program(info: &AccountView) -> Result<(), ProgramError> {
    if info.address().ne(&ATA_PROGRAM_ID) {
        return Err(ProgramError::IncorrectProgramId);
    }

    Ok(())
}

/// Verify account as current program, returning an error if it is not.
///
/// # Arguments
/// * `info` - The account to verify.
///
/// # Returns
/// * `Result<(), ProgramError>` - The result of the operation
#[inline(always)]
pub fn verify_current_program(info: &AccountView) -> Result<(), ProgramError> {
    if info.address().ne(&CONTRA_ESCROW_PROGRAM_ID) {
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
pub fn verify_token_programs(info: &AccountView) -> Result<(), ProgramError> {
    if info.address().ne(&TOKEN_PROGRAM_ID) && info.address().ne(&TOKEN_2022_PROGRAM_ID) {
        return Err(ProgramError::IncorrectProgramId);
    }

    Ok(())
}

/// Validates a Program Derived Address (PDA) against expected parameters.
///
/// # Arguments
/// * `seeds` - The seeds used to derive the PDA
/// * `program_id` - The program ID that should own the PDA
/// * `expected_bump` - The expected bump seed value
/// * `account_info` - The account that should match the derived PDA
///
/// # Returns
/// * `Result<Address, ProgramError>` - The validated PDA on success
#[inline(always)]
pub fn validate_pda_account(
    seeds: &[&[u8]],
    program_id: &Address,
    expected_bump: u8,
    account_info: &AccountView,
) -> Result<Address, ProgramError> {
    // Calculate the PDA
    let (calculated_pda, calculated_bump) = Address::find_program_address(seeds, program_id);

    // Validate bump matches expected
    if calculated_bump != expected_bump {
        return Err(ProgramError::InvalidInstructionData);
    }

    // Validate account key matches calculated PDA
    if account_info.address() != &calculated_pda {
        return Err(ProgramError::InvalidSeeds);
    }

    // Validate account is owned by the program (for initialized accounts)
    if !account_info.is_data_empty() && !account_info.owned_by(program_id) {
        return Err(ProgramError::InvalidAccountOwner);
    }

    Ok(calculated_pda)
}
