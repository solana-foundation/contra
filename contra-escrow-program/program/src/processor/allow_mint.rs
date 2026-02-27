extern crate alloc;

use crate::{
    constants::ALLOWED_MINT_SEED,
    error::ContraEscrowProgramError,
    events::AllowMintEvent,
    processor::{
        shared::{
            account_check::{verify_signer, verify_system_account, verify_system_program},
            event_utils::emit_event,
            pda_utils::create_pda_account,
            token_utils::{get_mint_decimals, get_or_create_ata},
        },
        validate_token2022_extensions, verify_account_owner, verify_ata_program,
        verify_current_program, verify_token_programs,
    },
    require_len,
    state::{discriminator::AccountSerialize, AllowedMint, Instance},
    validate_event_accounts,
};
use pinocchio::{
    account::AccountView,
    cpi::Seed,
    error::ProgramError,
    sysvars::{rent::Rent, Sysvar},
    Address, ProgramResult,
};
use pinocchio_token_2022::ID as TOKEN_2022_PROGRAM_ID;

/// Processes the AllowMint instruction.
///
/// # Account Layout
/// 0. `[signer, writable]` payer - Pays for the account creation
/// 1. `[signer]` admin - Admin of the instance
/// 2. `[]` instance - Instance PDA to validate admin authority
/// 3. `[]` mint - Token mint to be allowed
/// 4. `[writable]` allowed_mint - AllowedMint PDA to be created
/// 5. `[writable]` instance_ata - Instance PDA's ATA for this mint
/// 6. `[]` system_program - System program for account creation
/// 7. `[]` token_program - Token program for the mint
/// 8. `[signer]` event_authority - Event authority PDA for emitting events
/// 9. `[]` contra_escrow_program - Current program for CPI
///
/// # Instruction Data
/// * `bump` (u8) - Bump for the allowed mint PDA
pub fn process_allow_mint(
    program_id: &Address,
    accounts: &[AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let args = process_instruction_data(instruction_data)?;
    let [payer_info, admin_info, instance_info, mint_info, allowed_mint_info, instance_ata_info, system_program_info, token_program_info, associated_token_program_info, event_authority_info, program_info] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    // Validate account signatures and mutability
    verify_signer(payer_info, true)?;
    verify_signer(admin_info, false)?;

    verify_system_account(allowed_mint_info, true)?;

    // Verify programs
    verify_ata_program(associated_token_program_info)?;
    verify_system_program(system_program_info)?;
    verify_token_programs(token_program_info)?;
    verify_current_program(program_info)?;

    validate_event_accounts!(event_authority_info, program_info);

    // Validate mint (Token or Token2022, check for disallowed extensions)
    verify_account_owner(mint_info, token_program_info.address())?;
    if token_program_info.address() == &TOKEN_2022_PROGRAM_ID {
        validate_token2022_extensions(mint_info)?;
    }

    // Get mint decimals for event
    let mint_decimals = get_mint_decimals(mint_info)?;

    // Validate instance
    let instance_data = instance_info.try_borrow()?;
    let instance = Instance::try_from_bytes(&instance_data)?;

    instance
        .validate_pda(instance_info)
        .map_err(|_| ContraEscrowProgramError::InvalidInstance)?;

    instance.validate_admin(admin_info.address())?;

    // Create ATA for instance PDA and this mint (idempotent)
    get_or_create_ata(
        instance_ata_info,
        instance_info,
        mint_info,
        payer_info,
        system_program_info,
        token_program_info,
    )?;

    // Create AllowedMint struct and validate PDA
    let allowed_mint = AllowedMint::new(args.bump)?;
    allowed_mint
        .validate_pda(
            instance_info.address(),
            mint_info.address(),
            allowed_mint_info,
        )
        .map_err(|_| ContraEscrowProgramError::InvalidAllowedMint)?;

    let bump_seed = [args.bump];

    // Create the AllowedMint account
    let allowed_mint_seeds = [
        Seed::from(ALLOWED_MINT_SEED),
        Seed::from(instance_info.address().as_ref()),
        Seed::from(mint_info.address().as_ref()),
        Seed::from(&bump_seed),
    ];

    let rent = Rent::get()?;
    create_pda_account(
        payer_info,
        &rent,
        AllowedMint::LEN,
        program_id,
        allowed_mint_info,
        allowed_mint_seeds,
        None,
    )?;

    // Initialize AllowedMint data
    let allowed_mint_data = allowed_mint.to_bytes();

    // Write the serialized data to the account
    let mut data_slice = allowed_mint_info.try_borrow_mut()?;
    data_slice[..allowed_mint_data.len()].copy_from_slice(&allowed_mint_data);

    // Emit AllowMint event
    let event = AllowMintEvent::new(instance.instance_seed, *mint_info.address(), mint_decimals);
    emit_event(
        program_id,
        event_authority_info,
        program_info,
        &event.to_bytes(),
    )?;

    Ok(())
}

struct AllowMintArgs {
    bump: u8,
}

fn process_instruction_data(data: &[u8]) -> Result<AllowMintArgs, ProgramError> {
    require_len!(data, 1); // Only need: bump(1)

    // Read bump (first byte)
    let bump = data[0];

    Ok(AllowMintArgs { bump })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ID as CONTRA_ESCROW_PROGRAM_ID;
    use alloc::vec;

    #[test]
    fn test_process_allow_mint_valid_bump() {
        // Test with valid bump
        let instruction_data = vec![123]; // bump = 123

        let result = process_instruction_data(&instruction_data);

        assert!(result.is_ok());
        assert_eq!(result.unwrap().bump, 123);
    }

    #[test]
    fn test_process_allow_mint_empty_instruction_data() {
        let instruction_data = [];
        let accounts = [];

        let result = process_allow_mint(&CONTRA_ESCROW_PROGRAM_ID, &accounts, &instruction_data);

        assert_eq!(result.unwrap_err(), ProgramError::InvalidInstructionData);
    }
}
