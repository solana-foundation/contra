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
    permanent_delegate::PermanentDelegate, BaseStateWithExtensions,
};
use spl_token_2022::state::Mint as Token2022MintState;

use crate::error::ContraEscrowProgramError;

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

#[inline(always)]
pub fn get_token_account_balance(info: &AccountView) -> Result<u64, ProgramError> {
    if info.owned_by(&TOKEN_PROGRAM_ID) {
        let data = info.try_borrow()?;
        let account = unsafe { TokenAccount::from_bytes_unchecked(&data) };
        return Ok(account.amount());
    }
    if info.owned_by(&TOKEN_2022_PROGRAM_ID) {
        let data = info.try_borrow()?;
        let account = unsafe { Token2022Account::from_bytes_unchecked(&data) };
        return Ok(account.amount());
    }
    Err(ContraEscrowProgramError::InvalidTokenAccount.into())
}

#[inline(always)]
pub fn get_mint_decimals(mint_info: &AccountView) -> Result<u8, ProgramError> {
    if mint_info.owned_by(&TOKEN_PROGRAM_ID) {
        let data = mint_info.try_borrow()?;
        let mint = unsafe { TokenMint::from_bytes_unchecked(&data) };
        return Ok(mint.decimals());
    }
    if mint_info.owned_by(&TOKEN_2022_PROGRAM_ID) {
        let data = mint_info.try_borrow()?;
        let mint = unsafe { Token2022Mint::from_bytes_unchecked(&data) };
        return Ok(mint.decimals());
    }
    Err(ContraEscrowProgramError::InvalidMint.into())
}

/// Blocks mints with the PermanentDelegate extension. Pausable mints are
/// accepted — the operator checks the live pause state before withdrawal.
#[inline(always)]
pub fn validate_token2022_extensions(mint_info: &AccountView) -> ProgramResult {
    let data = mint_info.try_borrow()?;

    let mint = StateWithExtensions::<Token2022MintState>::unpack(&data)
        .map_err(|_| ContraEscrowProgramError::InvalidMint)?;

    if mint.get_extension::<PermanentDelegate>().is_ok() {
        return Err(ContraEscrowProgramError::PermanentDelegateNotAllowed.into());
    }

    Ok(())
}
