use crate::{
    pda_utils::{find_allowed_mint_pda, find_event_authority_pda},
    state_utils::{assert_get_or_allow_mint, assert_get_or_create_instance, assert_get_or_deposit},
    utils::{
        assert_program_error, set_mint, set_mint_2022_basic, set_mint_2022_with_permanent_delegate,
        setup_test_balances, TestContext, ATA_PROGRAM_ID, CONTRA_ESCROW_PROGRAM_ID,
        INVALID_ACCOUNT_DATA_ERROR, INVALID_INSTRUCTION_DATA_ERROR, NOT_ENOUGH_ACCOUNT_KEYS_ERROR,
        PERMANENT_DELEGATE_NOT_ALLOWED_ERROR, TOKEN_2022_PROGRAM_ID,
        TOKEN_INSUFFICIENT_FUNDS_ERROR,
    },
};

use contra_escrow_program_client::instructions::DepositBuilder;
use solana_sdk::{
    instruction::Instruction,
    signature::{Keypair, Signer},
    system_program::ID as SYSTEM_PROGRAM_ID,
};
use spl_associated_token_account::get_associated_token_address_with_program_id;
use spl_token::ID as TOKEN_PROGRAM_ID;

const DEPOSIT_AMOUNT: u64 = 1_000_000; // 1 token with 6 decimals

#[test]
fn test_deposit_success() {
    let mut context = TestContext::new();
    let admin = Keypair::new();
    let user = Keypair::new();
    let mint = Keypair::new();

    let instance_seed = Keypair::new();

    set_mint(&mut context, &mint.pubkey());

    let (instance_pda, _) =
        assert_get_or_create_instance(&mut context, &admin, &instance_seed, false, false)
            .expect("CreateInstance should succeed");

    assert_get_or_allow_mint(
        &mut context,
        &admin,
        &instance_pda,
        &mint.pubkey(),
        false,
        false,
    )
    .expect("AllowMint should succeed");

    setup_test_balances(
        &mut context,
        &user,
        &instance_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
        DEPOSIT_AMOUNT * 2,
        0,
    );

    assert_get_or_deposit(
        &mut context,
        &user,
        &instance_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
        DEPOSIT_AMOUNT,
        None,
        true,
    )
    .expect("Deposit should succeed");
}

#[test]
fn test_deposit_with_recipient() {
    let mut context = TestContext::new();
    let admin = Keypair::new();
    let user = Keypair::new();
    let recipient = Keypair::new();
    let mint = Keypair::new();

    let instance_seed = Keypair::new();

    set_mint(&mut context, &mint.pubkey());

    let (instance_pda, _) =
        assert_get_or_create_instance(&mut context, &admin, &instance_seed, false, false)
            .expect("CreateInstance should succeed");

    assert_get_or_allow_mint(
        &mut context,
        &admin,
        &instance_pda,
        &mint.pubkey(),
        false,
        false,
    )
    .expect("AllowMint should succeed");

    setup_test_balances(
        &mut context,
        &user,
        &instance_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
        DEPOSIT_AMOUNT * 2,
        0,
    );

    assert_get_or_deposit(
        &mut context,
        &user,
        &instance_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
        DEPOSIT_AMOUNT,
        Some(recipient.pubkey()),
        false,
    )
    .expect("Deposit with recipient should succeed");
}

#[test]
fn test_deposit_insufficient_funds() {
    let mut context = TestContext::new();
    let admin = Keypair::new();
    let user = Keypair::new();
    let mint = Keypair::new();

    let instance_seed = Keypair::new();

    set_mint(&mut context, &mint.pubkey());

    let (instance_pda, _) =
        assert_get_or_create_instance(&mut context, &admin, &instance_seed, false, false)
            .expect("CreateInstance should succeed");

    assert_get_or_allow_mint(
        &mut context,
        &admin,
        &instance_pda,
        &mint.pubkey(),
        false,
        false,
    )
    .expect("AllowMint should succeed");

    setup_test_balances(
        &mut context,
        &user,
        &instance_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
        DEPOSIT_AMOUNT / 2, // Not enough tokens
        0,
    );

    context
        .airdrop_if_required(&user.pubkey(), 1_000_000_000)
        .unwrap();

    let (allowed_mint_pda, _) = find_allowed_mint_pda(&instance_pda, &mint.pubkey());
    let (event_authority_pda, _) = find_event_authority_pda();

    let user_ata = get_associated_token_address_with_program_id(
        &user.pubkey(),
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
    );
    let instance_ata = get_associated_token_address_with_program_id(
        &instance_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
    );

    let instruction = DepositBuilder::new()
        .payer(context.payer.pubkey())
        .user(user.pubkey())
        .instance(instance_pda)
        .mint(mint.pubkey())
        .allowed_mint(allowed_mint_pda)
        .user_ata(user_ata)
        .instance_ata(instance_ata)
        .system_program(SYSTEM_PROGRAM_ID)
        .token_program(TOKEN_PROGRAM_ID)
        .associated_token_program(ATA_PROGRAM_ID)
        .event_authority(event_authority_pda)
        .contra_escrow_program(CONTRA_ESCROW_PROGRAM_ID)
        .amount(DEPOSIT_AMOUNT)
        .instruction();

    let result = context.send_transaction_with_signers(instruction, &[&user]);

    assert_program_error(result, TOKEN_INSUFFICIENT_FUNDS_ERROR);
}

#[test]
fn test_deposit_mint_not_allowed() {
    let mut context = TestContext::new();
    let admin = Keypair::new();
    let user = Keypair::new();
    let mint = Keypair::new();

    let instance_seed = Keypair::new();

    set_mint(&mut context, &mint.pubkey());

    let (instance_pda, _) =
        assert_get_or_create_instance(&mut context, &admin, &instance_seed, false, false)
            .expect("CreateInstance should succeed");

    setup_test_balances(
        &mut context,
        &user,
        &instance_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
        DEPOSIT_AMOUNT,
        0,
    );

    context
        .airdrop_if_required(&user.pubkey(), 1_000_000_000)
        .unwrap();

    let (allowed_mint_pda, _) = find_allowed_mint_pda(&instance_pda, &mint.pubkey());
    let (event_authority_pda, _) = find_event_authority_pda();

    let user_ata = get_associated_token_address_with_program_id(
        &user.pubkey(),
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
    );
    let instance_ata = get_associated_token_address_with_program_id(
        &instance_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
    );

    let instruction = DepositBuilder::new()
        .payer(context.payer.pubkey())
        .user(user.pubkey())
        .instance(instance_pda)
        .mint(mint.pubkey())
        .allowed_mint(allowed_mint_pda)
        .user_ata(user_ata)
        .instance_ata(instance_ata)
        .system_program(SYSTEM_PROGRAM_ID)
        .token_program(TOKEN_PROGRAM_ID)
        .associated_token_program(ATA_PROGRAM_ID)
        .event_authority(event_authority_pda)
        .contra_escrow_program(CONTRA_ESCROW_PROGRAM_ID)
        .amount(DEPOSIT_AMOUNT)
        .instruction();

    let result = context.send_transaction_with_signers(instruction, &[&user]);

    assert_program_error(result, INVALID_ACCOUNT_DATA_ERROR);
}

#[test]
fn test_deposit_invalid_instruction_data_too_short() {
    let mut context = TestContext::new();

    let instruction = Instruction {
        program_id: CONTRA_ESCROW_PROGRAM_ID,
        accounts: vec![],
        data: vec![6, 1, 2], // Too short instruction data
    };

    let result = context.send_transaction(instruction);
    assert_program_error(result, INVALID_INSTRUCTION_DATA_ERROR);
}

#[test]
fn test_deposit_not_enough_accounts() {
    let mut context = TestContext::new();

    let instruction = Instruction {
        program_id: CONTRA_ESCROW_PROGRAM_ID,
        accounts: vec![], // No accounts
        // 1 discriminator + 8 amount + 1 recipient option
        data: vec![6, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    };

    let result = context.send_transaction(instruction);
    assert_program_error(result, NOT_ENOUGH_ACCOUNT_KEYS_ERROR);
}

// Token 2022 Tests

#[test]
fn test_deposit_token_2022_basic_success() {
    let mut context = TestContext::new();
    let admin = Keypair::new();
    let user = Keypair::new();
    let mint = Keypair::new();

    let instance_seed = Keypair::new();

    set_mint_2022_basic(&mut context, &mint.pubkey());

    let (instance_pda, _) =
        assert_get_or_create_instance(&mut context, &admin, &instance_seed, false, false)
            .expect("CreateInstance should succeed");

    assert_get_or_allow_mint(
        &mut context,
        &admin,
        &instance_pda,
        &mint.pubkey(),
        false,
        false,
    )
    .expect("AllowMint should succeed");

    setup_test_balances(
        &mut context,
        &user,
        &instance_pda,
        &mint.pubkey(),
        &TOKEN_2022_PROGRAM_ID,
        DEPOSIT_AMOUNT,
        0,
    );

    assert_get_or_deposit(
        &mut context,
        &user,
        &instance_pda,
        &mint.pubkey(),
        &TOKEN_2022_PROGRAM_ID,
        DEPOSIT_AMOUNT,
        None,
        false,
    )
    .expect("Token2022 deposit should succeed");
}

#[test]
fn test_deposit_token_2022_permanent_delegate_rejected() {
    let mut context = TestContext::new();
    let admin = Keypair::new();
    let user = Keypair::new();
    let good_mint = Keypair::new();
    let bad_mint = Keypair::new();

    let instance_seed = Keypair::new();

    // Step 1: Create a normal Token2022 mint without permanent delegate
    set_mint_2022_basic(&mut context, &good_mint.pubkey());

    // Step 2: Create instance and allow the good mint
    let (instance_pda, _) =
        assert_get_or_create_instance(&mut context, &admin, &instance_seed, false, false)
            .expect("CreateInstance should succeed");

    let (allowed_mint_pda, _) = assert_get_or_allow_mint(
        &mut context,
        &admin,
        &instance_pda,
        &good_mint.pubkey(),
        false,
        false,
    )
    .expect("AllowMint should succeed for normal mint");

    // Step 3: Set up deposit test with good mint
    setup_test_balances(
        &mut context,
        &user,
        &instance_pda,
        &good_mint.pubkey(),
        &TOKEN_2022_PROGRAM_ID,
        DEPOSIT_AMOUNT,
        0,
    );

    // Step 4: Create a bad mint with permanent delegate extension (we only need its account data)
    set_mint_2022_with_permanent_delegate(&mut context, &bad_mint.pubkey());

    // Step 5: Use LiteSVM cheat code to replace the good mint's account data with bad mint data
    // This simulates a scenario where a legitimate mint gets compromised with permanent delegate
    let bad_mint_account = context
        .get_account(&bad_mint.pubkey())
        .expect("Bad mint account should exist");

    // Replace the good mint account with bad mint account data (which has permanent delegate)
    context
        .svm
        .set_account(good_mint.pubkey(), bad_mint_account)
        .expect("Failed to set good mint account with bad mint data");

    // Step 6: Try to deposit - should fail because good_mint now has permanent delegate data
    context
        .airdrop_if_required(&user.pubkey(), 1_000_000_000)
        .unwrap();
    let (event_authority_pda, _) = find_event_authority_pda();

    let user_ata = get_associated_token_address_with_program_id(
        &user.pubkey(),
        &good_mint.pubkey(), // Use good mint (the one we originally set up)
        &TOKEN_2022_PROGRAM_ID,
    );
    let instance_ata = get_associated_token_address_with_program_id(
        &instance_pda,
        &good_mint.pubkey(), // Use good mint (the one we originally set up)
        &TOKEN_2022_PROGRAM_ID,
    );

    let instruction = DepositBuilder::new()
        .payer(context.payer.pubkey())
        .user(user.pubkey())
        .instance(instance_pda)
        .mint(good_mint.pubkey()) // Use good mint (but it now has bad mint data)
        .allowed_mint(allowed_mint_pda) // AllowedMint for good mint
        .user_ata(user_ata)
        .instance_ata(instance_ata)
        .system_program(SYSTEM_PROGRAM_ID)
        .token_program(TOKEN_2022_PROGRAM_ID)
        .associated_token_program(ATA_PROGRAM_ID)
        .event_authority(event_authority_pda)
        .contra_escrow_program(CONTRA_ESCROW_PROGRAM_ID)
        .amount(DEPOSIT_AMOUNT)
        .instruction();

    let result = context.send_transaction_with_signers(instruction, &[&user]);

    assert_program_error(result, PERMANENT_DELEGATE_NOT_ALLOWED_ERROR);
}
