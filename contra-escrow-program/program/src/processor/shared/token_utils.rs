use pinocchio::{account::AccountView, address::Address, error::ProgramError, ProgramResult};
use pinocchio_associated_token_account::instructions::CreateIdempotent;
use pinocchio_token::{
    state::{Mint as TokenMint, TokenAccount},
    ID as TOKEN_PROGRAM_ID,
};
use pinocchio_token_2022::{
    state::Mint as Token2022Mint, state::TokenAccount as Token2022Account,
    ID as TOKEN_2022_PROGRAM_ID,
};
use spl_token_2022::extension::StateWithExtensions;
use spl_token_2022::extension::{
    pausable::PausableConfig, permanent_delegate::PermanentDelegate, BaseStateWithExtensions,
};
use spl_token_2022::state::Mint as Token2022MintState;

use crate::error::ContraEscrowProgramError;

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

/// Validates an Associated Token Account address and creates it if it doesn't exist.
///
/// # Arguments
/// * `ata_info` - The ATA account to validate/create
/// * `wallet_info` - The wallet that should own the ATA
/// * `mint_info` - The token mint for the ATA
/// * `payer_info` - The account paying for creation (if needed)
/// * `system_program_info` - The system program account
/// * `token_program_info` - The token program account
///
/// # Returns
/// * `ProgramResult` - Success if validation passes and creation (if needed) succeeds
#[inline(always)]
pub fn get_or_create_ata(
    ata_info: &AccountView,
    wallet_info: &AccountView,
    mint_info: &AccountView,
    payer_info: &AccountView,
    system_program_info: &AccountView,
    token_program_info: &AccountView,
) -> ProgramResult {
    // Validate ATA address is correct for this wallet + mint
    let expected_ata = Address::find_program_address(
        &[
            wallet_info.address().as_ref(),
            token_program_info.address().as_ref(),
            mint_info.address().as_ref(),
        ],
        &pinocchio_associated_token_account::ID,
    )
    .0;

    if ata_info.address() != &expected_ata {
        return Err(ContraEscrowProgramError::InvalidAta.into());
    }

    // Create ATA if it doesn't exist
    if ata_info.is_data_empty() {
        CreateIdempotent {
            funding_account: payer_info,
            account: ata_info,
            wallet: wallet_info,
            mint: mint_info,
            system_program: system_program_info,
            token_program: token_program_info,
        }
        .invoke()?;
    }

    Ok(())
}

// Get token account balance
#[inline(always)]
pub fn get_token_account_balance(info: &AccountView) -> Result<u64, ProgramError> {
    if info.owned_by(&TOKEN_PROGRAM_ID) {
        let data = info.try_borrow()?;
        let account = unsafe { TokenAccount::from_bytes_unchecked(&data) };
        return Ok(account.amount());
    } else if info.owned_by(&TOKEN_2022_PROGRAM_ID) {
        let data = info.try_borrow()?;
        let account = unsafe { Token2022Account::from_bytes_unchecked(&data) };
        return Ok(account.amount());
    }

    Err(ContraEscrowProgramError::InvalidTokenAccount.into())
}

/// Get mint decimals for either Token or Token2022 program.
///
/// # Arguments
/// * `mint_info` - The mint account to read decimals from
///
/// # Returns
/// * `Result<u8, ProgramError>` - The mint decimals
#[inline(always)]
pub fn get_mint_decimals(mint_info: &AccountView) -> Result<u8, ProgramError> {
    if mint_info.owned_by(&TOKEN_PROGRAM_ID) {
        let data = mint_info.try_borrow()?;
        let mint = unsafe { TokenMint::from_bytes_unchecked(&data) };
        return Ok(mint.decimals());
    } else if mint_info.owned_by(&TOKEN_2022_PROGRAM_ID) {
        let data = mint_info.try_borrow()?;
        let mint = unsafe { Token2022Mint::from_bytes_unchecked(&data) };
        return Ok(mint.decimals());
    }

    Err(ContraEscrowProgramError::InvalidMint.into())
}

/// Checks Token2022 mints for extensions that we want to block.
/// Currently blocks mints with PermanentDelegate or pausable capabilities.
///
/// # Arguments
/// * `mint_info` - The Token2022 mint account to check
///
/// # Returns
/// * `ProgramResult` - Success if no dangerous extensions, error if dangerous extensions found
#[inline(always)]
pub fn validate_token2022_extensions(mint_info: &AccountView) -> ProgramResult {
    let data = mint_info.try_borrow()?;

    // Parse mint with extensions directly
    let mint = StateWithExtensions::<Token2022MintState>::unpack(&data)
        .map_err(|_| ContraEscrowProgramError::InvalidMint)?;

    // Check for PermanentDelegate extension
    if let Ok(_permanent_delegate) = mint.get_extension::<PermanentDelegate>() {
        return Err(ContraEscrowProgramError::PermanentDelegateNotAllowed.into());
    }

    // Check for PausableConfig extension (pausable mint)
    if let Ok(_pausable_config) = mint.get_extension::<PausableConfig>() {
        return Err(ContraEscrowProgramError::PausableMintNotAllowed.into());
    }

    Ok(())
}
