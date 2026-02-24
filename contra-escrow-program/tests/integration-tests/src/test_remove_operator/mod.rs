use crate::{
    pda_utils::{find_event_authority_pda, find_operator_pda},
    state_utils::{
        assert_get_or_add_operator, assert_get_or_create_instance, assert_get_or_remove_operator,
    },
    utils::{
        assert_program_error, TestContext, CONTRA_ESCROW_PROGRAM_ID, INVALID_ACCOUNT_DATA_ERROR,
        INVALID_ADMIN_ERROR, MISSING_REQUIRED_SIGNATURE_ERROR,
    },
};
use contra_escrow_program_client::instructions::RemoveOperatorBuilder;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    signature::{Keypair, Signer},
    system_program::ID as SYSTEM_PROGRAM_ID,
};

#[test]
fn test_remove_operator_success() {
    let mut context = TestContext::new();
    let admin = Keypair::new();
    let operator_wallet = Keypair::new();

    let instance_seed = Keypair::new();

    let (instance_pda, _) =
        assert_get_or_create_instance(&mut context, &admin, &instance_seed, false, false)
            .expect("CreateInstance should succeed");

    let (operator_pda, _) = assert_get_or_add_operator(
        &mut context,
        &admin,
        &instance_pda,
        &operator_wallet.pubkey(),
        false,
        false,
    )
    .expect("AddOperator should succeed");

    assert_get_or_remove_operator(
        &mut context,
        &admin,
        &instance_pda,
        &operator_wallet.pubkey(),
        &operator_pda,
        true,
    )
    .expect("RemoveOperator should succeed");
}

#[test]
fn test_remove_operator_nonexistent() {
    let mut context = TestContext::new();
    let admin = Keypair::new();
    let operator_wallet = Keypair::new();

    let instance_seed = Keypair::new();

    let (instance_pda, _) =
        assert_get_or_create_instance(&mut context, &admin, &instance_seed, false, false)
            .expect("CreateInstance should succeed");

    context
        .airdrop_if_required(&admin.pubkey(), 1_000_000_000)
        .unwrap();

    let (operator_pda, _) = find_operator_pda(&instance_pda, &operator_wallet.pubkey());
    let (event_authority_pda, _) = find_event_authority_pda();

    let instruction = RemoveOperatorBuilder::new()
        .payer(context.payer.pubkey())
        .admin(admin.pubkey())
        .instance(instance_pda)
        .operator(operator_wallet.pubkey())
        .operator_pda(operator_pda)
        .system_program(SYSTEM_PROGRAM_ID)
        .event_authority(event_authority_pda)
        .contra_escrow_program(CONTRA_ESCROW_PROGRAM_ID)
        .instruction();

    let result = context.send_transaction_with_signers(instruction, &[&admin]);

    // Should fail because operator account doesn't exist
    assert_program_error(result, INVALID_ACCOUNT_DATA_ERROR);
}

#[test]
fn test_remove_operator_invalid_admin_not_signer() {
    let mut context = TestContext::new();
    let admin = Keypair::new();
    let operator_wallet = Keypair::new();

    let instance_seed = Keypair::new();

    let (instance_pda, _) =
        assert_get_or_create_instance(&mut context, &admin, &instance_seed, false, false)
            .expect("CreateInstance should succeed");

    let (operator_pda, _) = assert_get_or_add_operator(
        &mut context,
        &admin,
        &instance_pda,
        &operator_wallet.pubkey(),
        false,
        false,
    )
    .expect("AddOperator should succeed");

    context
        .airdrop_if_required(&admin.pubkey(), 1_000_000_000)
        .unwrap();

    let (event_authority_pda, _) = find_event_authority_pda();

    // Create instruction where admin is NOT marked as signer
    let accounts = vec![
        AccountMeta::new(context.payer.pubkey(), true), // payer (signer, writable)
        AccountMeta::new_readonly(admin.pubkey(), false), // admin (NOT signer)
        AccountMeta::new_readonly(instance_pda, false), // instance
        AccountMeta::new_readonly(operator_wallet.pubkey(), false), // operator
        AccountMeta::new(operator_pda, false),          // operator_pda (writable)
        AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false), // system_program
        AccountMeta::new_readonly(event_authority_pda, false), // event_authority
        AccountMeta::new_readonly(CONTRA_ESCROW_PROGRAM_ID, false), // contra_escrow_program
    ];

    let data = vec![4]; // discriminator for RemoveOperator

    let instruction = Instruction {
        program_id: CONTRA_ESCROW_PROGRAM_ID,
        accounts,
        data,
    };

    let result = context.send_transaction_with_signers(instruction, &[]);

    assert_program_error(result, MISSING_REQUIRED_SIGNATURE_ERROR);
}

#[test]
fn test_remove_operator_invalid_admin() {
    let mut context = TestContext::new();
    let admin = Keypair::new();
    let wrong_admin = Keypair::new();
    let operator_wallet = Keypair::new();

    let instance_seed = Keypair::new();

    let (instance_pda, _) =
        assert_get_or_create_instance(&mut context, &admin, &instance_seed, false, false)
            .expect("CreateInstance should succeed");

    let (operator_pda, _) = assert_get_or_add_operator(
        &mut context,
        &admin,
        &instance_pda,
        &operator_wallet.pubkey(),
        false,
        false,
    )
    .expect("AddOperator should succeed");

    context
        .airdrop_if_required(&wrong_admin.pubkey(), 1_000_000_000)
        .unwrap();

    let (event_authority_pda, _) = find_event_authority_pda();

    let instruction = RemoveOperatorBuilder::new()
        .payer(context.payer.pubkey())
        .admin(wrong_admin.pubkey())
        .instance(instance_pda)
        .operator(operator_wallet.pubkey())
        .operator_pda(operator_pda)
        .system_program(SYSTEM_PROGRAM_ID)
        .event_authority(event_authority_pda)
        .contra_escrow_program(CONTRA_ESCROW_PROGRAM_ID)
        .instruction();

    let result = context.send_transaction_with_signers(instruction, &[&wrong_admin]);

    assert_program_error(result, INVALID_ADMIN_ERROR);
}

#[test]
fn test_remove_operator_invalid_instance_account_owner() {
    let mut context = TestContext::new();
    let admin = Keypair::new();
    let operator_wallet = Keypair::new();

    context
        .airdrop_if_required(&admin.pubkey(), 1_000_000_000)
        .unwrap();

    // Will be system account so will have invalid account owner
    // We don't even need to create an instance or an operator, as this check is at the beginning of the instruction
    let fake_instance = Keypair::new();
    context
        .airdrop_if_required(&fake_instance.pubkey(), 1_000_000_000)
        .unwrap();

    let (operator_pda, _) = find_operator_pda(&fake_instance.pubkey(), &operator_wallet.pubkey());
    let (event_authority_pda, _) = find_event_authority_pda();

    let instruction = RemoveOperatorBuilder::new()
        .payer(context.payer.pubkey())
        .admin(admin.pubkey())
        .instance(fake_instance.pubkey())
        .operator(operator_wallet.pubkey())
        .operator_pda(operator_pda)
        .system_program(SYSTEM_PROGRAM_ID)
        .event_authority(event_authority_pda)
        .contra_escrow_program(CONTRA_ESCROW_PROGRAM_ID)
        .instruction();

    let result = context.send_transaction_with_signers(instruction, &[&admin]);

    assert_program_error(result, INVALID_ACCOUNT_DATA_ERROR);
}
