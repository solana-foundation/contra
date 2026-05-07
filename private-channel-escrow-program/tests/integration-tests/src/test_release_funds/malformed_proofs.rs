use crate::{
    pda_utils::{find_allowed_mint_pda, find_event_authority_pda},
    smt_utils::{ProcessorSMT, MAX_TREE_LEAVES},
    state_utils::{
        assert_get_or_add_operator, assert_get_or_allow_mint, assert_get_or_create_instance,
        assert_get_or_release_funds,
    },
    utils::{
        assert_program_error, set_mint, setup_test_balances, TestContext, ATA_PROGRAM_ID,
        INVALID_INSTRUCTION_DATA_ERROR, INVALID_SMT_PROOF_ERROR,
        INVALID_TRANSACTION_NONCE_FOR_CURRENT_TREE_INDEX_ERROR, PRIVATE_CHANNEL_ESCROW_PROGRAM_ID,
    },
};

use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    signature::{Keypair, Signer},
};
use spl_associated_token_account::get_associated_token_address_with_program_id;
use spl_token::ID as TOKEN_PROGRAM_ID;

const LARGE_DEPOSIT: u64 = 10_000_000;
const RELEASE_AMOUNT: u64 = 100_000;

fn setup_release_context() -> (
    TestContext,
    Keypair,                    // operator
    solana_sdk::pubkey::Pubkey, // instance_pda
    solana_sdk::pubkey::Pubkey, // operator_pda
    Keypair,                    // mint
    Keypair,                    // user
) {
    let mut context = TestContext::new();
    let admin = Keypair::new();
    let operator = Keypair::new();
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

    let (operator_pda, _) = assert_get_or_add_operator(
        &mut context,
        &admin,
        &instance_pda,
        &operator.pubkey(),
        false,
        false,
    )
    .expect("AddOperator should succeed");

    setup_test_balances(
        &mut context,
        &user,
        &instance_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
        0,
        LARGE_DEPOSIT,
    );

    (context, operator, instance_pda, operator_pda, mint, user)
}

#[test]
fn test_malformed_proof_all_zero_siblings() {
    let (mut context, operator, instance_pda, operator_pda, mint, user) = setup_release_context();

    let nonce: u64 = 42;
    let all_zero_proofs = [0u8; 512];

    let mut smt = ProcessorSMT::new();
    smt.insert(nonce);
    let new_root = smt.current_root();

    let result = assert_get_or_release_funds(
        &mut context,
        &operator,
        &instance_pda,
        &operator_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
        RELEASE_AMOUNT,
        &user.pubkey(),
        new_root,
        nonce,
        all_zero_proofs,
        false,
    );

    assert_program_error(result, INVALID_SMT_PROOF_ERROR);
}

#[test]
fn test_malformed_proof_wrong_nonce_siblings() {
    let (mut context, operator, instance_pda, operator_pda, mint, user) = setup_release_context();

    let nonce: u64 = 42;
    let wrong_nonce: u64 = 999;

    // Insert a nonce first so the tree is non-empty — in an empty tree all nonce
    // paths produce identical sibling proofs, so wrong-nonce proofs would pass.
    let setup_nonce: u64 = 500;
    let mut setup_smt = ProcessorSMT::new();
    let (_, setup_proofs) = setup_smt.generate_exclusion_proof_for_verification(setup_nonce);
    setup_smt.insert(setup_nonce);
    let setup_root = setup_smt.current_root();

    assert_get_or_release_funds(
        &mut context,
        &operator,
        &instance_pda,
        &operator_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
        RELEASE_AMOUNT,
        &user.pubkey(),
        setup_root,
        setup_nonce,
        setup_proofs,
        false,
    )
    .expect("Setup release should succeed");

    // Generate exclusion proof for wrong_nonce against the non-empty tree,
    // then submit it for a different nonce
    let mut smt = setup_smt;
    let (_, wrong_proofs) = smt.generate_exclusion_proof_for_verification(wrong_nonce);
    smt.insert(nonce);
    let new_root = smt.current_root();

    let result = assert_get_or_release_funds(
        &mut context,
        &operator,
        &instance_pda,
        &operator_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
        RELEASE_AMOUNT,
        &user.pubkey(),
        new_root,
        nonce,
        wrong_proofs,
        false,
    );

    assert_program_error(result, INVALID_SMT_PROOF_ERROR);
}

#[test]
fn test_malformed_proof_nonce_outside_tree_range() {
    let (mut context, operator, instance_pda, operator_pda, mint, user) = setup_release_context();

    let nonce: u64 = MAX_TREE_LEAVES as u64;

    let mut smt = ProcessorSMT::new();
    let (_, proofs) = smt.generate_exclusion_proof_for_verification(nonce);
    smt.insert(nonce);
    let new_root = smt.current_root();

    let result = assert_get_or_release_funds(
        &mut context,
        &operator,
        &instance_pda,
        &operator_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
        RELEASE_AMOUNT,
        &user.pubkey(),
        new_root,
        nonce,
        proofs,
        false,
    );

    assert_program_error(
        result,
        INVALID_TRANSACTION_NONCE_FOR_CURRENT_TREE_INDEX_ERROR,
    );
}

#[test]
fn test_malformed_proof_nonce_far_outside_range() {
    let (mut context, operator, instance_pda, operator_pda, mint, user) = setup_release_context();

    let nonce: u64 = MAX_TREE_LEAVES as u64 * 100;

    let mut smt = ProcessorSMT::new();
    let (_, proofs) = smt.generate_exclusion_proof_for_verification(nonce);
    smt.insert(nonce);
    let new_root = smt.current_root();

    let result = assert_get_or_release_funds(
        &mut context,
        &operator,
        &instance_pda,
        &operator_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
        RELEASE_AMOUNT,
        &user.pubkey(),
        new_root,
        nonce,
        proofs,
        false,
    );

    assert_program_error(
        result,
        INVALID_TRANSACTION_NONCE_FOR_CURRENT_TREE_INDEX_ERROR,
    );
}

#[test]
fn test_malformed_proof_truncated_siblings() {
    let (mut context, operator, instance_pda, operator_pda, mint, user) = setup_release_context();

    let nonce: u64 = 42;

    let mut smt = ProcessorSMT::new();
    let (_, sibling_proofs) = smt.generate_exclusion_proof_for_verification(nonce);
    smt.insert(nonce);
    let new_root = smt.current_root();

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

    let accounts = vec![
        AccountMeta::new(context.payer.pubkey(), true),
        AccountMeta::new_readonly(operator.pubkey(), true),
        AccountMeta::new(instance_pda, false),
        AccountMeta::new_readonly(operator_pda, false),
        AccountMeta::new_readonly(mint.pubkey(), false),
        AccountMeta::new_readonly(allowed_mint_pda, false),
        AccountMeta::new(user_ata, false),
        AccountMeta::new(instance_ata, false),
        AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
        AccountMeta::new_readonly(ATA_PROGRAM_ID, false),
        AccountMeta::new_readonly(event_authority_pda, false),
        AccountMeta::new_readonly(PRIVATE_CHANNEL_ESCROW_PROGRAM_ID, false),
    ];

    // 15 siblings (480 bytes) instead of the required 16 (512 bytes)
    let mut data = vec![7];
    data.extend_from_slice(&RELEASE_AMOUNT.to_le_bytes());
    data.extend_from_slice(user.pubkey().as_ref());
    data.extend_from_slice(&new_root);
    data.extend_from_slice(&nonce.to_le_bytes());
    data.extend_from_slice(&sibling_proofs[..480]);

    let instruction = Instruction {
        program_id: PRIVATE_CHANNEL_ESCROW_PROGRAM_ID,
        accounts,
        data,
    };

    let result = context.send_transaction_with_signers(instruction, &[&operator]);
    assert_program_error(result, INVALID_INSTRUCTION_DATA_ERROR);
}

#[test]
fn test_boundary_nonce_last_valid_for_tree_index_zero() {
    let (mut context, operator, instance_pda, operator_pda, mint, user) = setup_release_context();

    let nonce: u64 = MAX_TREE_LEAVES as u64 - 1;

    let mut smt = ProcessorSMT::new();
    let (_, sibling_proofs) = smt.generate_exclusion_proof_for_verification(nonce);
    smt.insert(nonce);
    let new_root = smt.current_root();

    assert_get_or_release_funds(
        &mut context,
        &operator,
        &instance_pda,
        &operator_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
        RELEASE_AMOUNT,
        &user.pubkey(),
        new_root,
        nonce,
        sibling_proofs,
        false,
    )
    .expect("Last valid nonce for tree_index=0 should succeed");
}

#[test]
fn test_zero_amount_release() {
    let (mut context, operator, instance_pda, operator_pda, mint, user) = setup_release_context();

    let nonce: u64 = 42;

    let mut smt = ProcessorSMT::new();
    let (_, sibling_proofs) = smt.generate_exclusion_proof_for_verification(nonce);
    smt.insert(nonce);
    let new_root = smt.current_root();

    assert_get_or_release_funds(
        &mut context,
        &operator,
        &instance_pda,
        &operator_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
        0,
        &user.pubkey(),
        new_root,
        nonce,
        sibling_proofs,
        false,
    )
    .expect("Zero-amount release succeeds — processor does not validate amount > 0");
}
