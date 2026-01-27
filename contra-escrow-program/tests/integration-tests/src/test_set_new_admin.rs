use crate::{
    pda_utils::find_event_authority_pda,
    state_utils::{assert_get_or_create_instance, assert_get_or_set_new_admin},
    utils::{
        assert_program_error, TestContext, CONTRA_ESCROW_PROGRAM_ID, INVALID_ACCOUNT_DATA_ERROR,
        INVALID_ADMIN_ERROR, MISSING_REQUIRED_SIGNATURE_ERROR,
    },
};
use contra_escrow_program_client::instructions::SetNewAdminBuilder;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    signature::{Keypair, Signer},
};

#[test]
fn test_set_new_admin_success() {
    let mut context = TestContext::new();
    let admin = Keypair::new();
    let new_admin = Keypair::new();

    let instance_seed = Keypair::new();

    let (instance_pda, _) =
        assert_get_or_create_instance(&mut context, &admin, &instance_seed, false, false)
            .expect("CreateInstance should succeed");

    assert_get_or_set_new_admin(&mut context, &admin, &instance_pda, &new_admin, true)
        .expect("SetNewAdmin should succeed");
}

#[test]
fn test_set_new_admin_invalid_current_admin_not_signer() {
    let mut context = TestContext::new();
    let admin = Keypair::new();
    let new_admin = Keypair::new();

    let instance_seed = Keypair::new();

    let (instance_pda, _) =
        assert_get_or_create_instance(&mut context, &admin, &instance_seed, false, false)
            .expect("CreateInstance should succeed");

    context
        .airdrop_if_required(&admin.pubkey(), 1_000_000_000)
        .unwrap();

    let (event_authority_pda, _) = find_event_authority_pda();

    // Create instruction where current_admin is NOT marked as signer (but new_admin is)
    let accounts = vec![
        AccountMeta::new(context.payer.pubkey(), true), // payer (signer, writable)
        AccountMeta::new_readonly(admin.pubkey(), false), // current_admin (NOT signer)
        AccountMeta::new(instance_pda, false),          // instance (writable)
        AccountMeta::new_readonly(new_admin.pubkey(), true), // new_admin (signer)
        AccountMeta::new_readonly(event_authority_pda, false), // event_authority
        AccountMeta::new_readonly(CONTRA_ESCROW_PROGRAM_ID, false), // contra_escrow_program
    ];

    let data = vec![5]; // discriminator for SetNewAdmin

    let instruction = Instruction {
        program_id: CONTRA_ESCROW_PROGRAM_ID,
        accounts,
        data,
    };

    let result = context.send_transaction_with_signers(instruction, &[&new_admin]);

    assert_program_error(result, MISSING_REQUIRED_SIGNATURE_ERROR);
}

#[test]
fn test_set_new_admin_invalid_current_admin() {
    let mut context = TestContext::new();
    let admin = Keypair::new();
    let wrong_admin = Keypair::new();
    let new_admin = Keypair::new();

    let instance_seed = Keypair::new();

    let (instance_pda, _) =
        assert_get_or_create_instance(&mut context, &admin, &instance_seed, false, false)
            .expect("CreateInstance should succeed");

    context
        .airdrop_if_required(&wrong_admin.pubkey(), 1_000_000_000)
        .unwrap();

    let (event_authority_pda, _) = find_event_authority_pda();

    let instruction = SetNewAdminBuilder::new()
        .payer(context.payer.pubkey())
        .current_admin(wrong_admin.pubkey())
        .instance(instance_pda)
        .new_admin(new_admin.pubkey())
        .event_authority(event_authority_pda)
        .contra_escrow_program(CONTRA_ESCROW_PROGRAM_ID)
        .instruction();

    let result = context.send_transaction_with_signers(instruction, &[&wrong_admin, &new_admin]);

    assert_program_error(result, INVALID_ADMIN_ERROR);
}

#[test]
fn test_set_new_admin_invalid_instance_account_owner() {
    let mut context = TestContext::new();
    let admin = Keypair::new();
    let new_admin = Keypair::new();

    context
        .airdrop_if_required(&admin.pubkey(), 1_000_000_000)
        .unwrap();

    let fake_instance = Keypair::new();
    context
        .airdrop_if_required(&fake_instance.pubkey(), 1_000_000_000)
        .unwrap();

    let (event_authority_pda, _) = find_event_authority_pda();

    let instruction = SetNewAdminBuilder::new()
        .payer(context.payer.pubkey())
        .current_admin(admin.pubkey())
        .instance(fake_instance.pubkey())
        .new_admin(new_admin.pubkey())
        .event_authority(event_authority_pda)
        .contra_escrow_program(CONTRA_ESCROW_PROGRAM_ID)
        .instruction();

    let result = context.send_transaction_with_signers(instruction, &[&admin, &new_admin]);

    assert_program_error(result, INVALID_ACCOUNT_DATA_ERROR);
}

#[test]
fn test_set_new_admin_invalid_new_admin_not_signer() {
    let mut context = TestContext::new();
    let admin = Keypair::new();
    let new_admin = Keypair::new();

    let instance_seed = Keypair::new();

    let (instance_pda, _) =
        assert_get_or_create_instance(&mut context, &admin, &instance_seed, false, false)
            .expect("CreateInstance should succeed");

    context
        .airdrop_if_required(&admin.pubkey(), 1_000_000_000)
        .unwrap();
    context
        .airdrop_if_required(&new_admin.pubkey(), 1_000_000_000)
        .unwrap();

    let (event_authority_pda, _) = find_event_authority_pda();

    // Create instruction where new_admin is NOT marked as signer
    let accounts = vec![
        AccountMeta::new(context.payer.pubkey(), true), // payer (signer, writable)
        AccountMeta::new_readonly(admin.pubkey(), true), // current_admin (signer)
        AccountMeta::new(instance_pda, false),          // instance (writable)
        AccountMeta::new_readonly(new_admin.pubkey(), false), // new_admin (NOT signer)
        AccountMeta::new_readonly(event_authority_pda, false), // event_authority
        AccountMeta::new_readonly(CONTRA_ESCROW_PROGRAM_ID, false), // contra_escrow_program
    ];

    let data = vec![5]; // discriminator for SetNewAdmin

    let instruction = Instruction {
        program_id: CONTRA_ESCROW_PROGRAM_ID,
        accounts,
        data,
    };

    let result = context.send_transaction_with_signers(instruction, &[&admin]);

    assert_program_error(result, MISSING_REQUIRED_SIGNATURE_ERROR);
}
