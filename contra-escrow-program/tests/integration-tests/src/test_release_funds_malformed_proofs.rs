use crate::{
    smt_utils::{ProcessorSMT, MAX_TREE_LEAVES},
    state_utils::{
        assert_get_or_add_operator, assert_get_or_allow_mint, assert_get_or_create_instance,
        assert_get_or_release_funds,
    },
    utils::{
        assert_program_error, set_mint, setup_test_balances, TestContext,
        INVALID_INSTRUCTION_DATA_ERROR, INVALID_SMT_PROOF_ERROR,
        INVALID_TRANSACTION_NONCE_FOR_CURRENT_TREE_INDEX_ERROR,
    },
};

use solana_sdk::signature::{Keypair, Signer};
use spl_token::ID as TOKEN_PROGRAM_ID;

const LARGE_DEPOSIT: u64 = 10_000_000;
const RELEASE_AMOUNT: u64 = 100_000;

/// Helper: set up a fully initialized escrow context ready for release_funds tests.
fn setup_release_context() -> (
    TestContext,
    Keypair,       // operator
    solana_sdk::pubkey::Pubkey, // instance_pda
    solana_sdk::pubkey::Pubkey, // operator_pda
    Keypair,       // mint
    Keypair,       // user
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

/// T010-1: Truncated proof data — all-zero sibling proofs (invalid).
#[test]
fn test_malformed_proof_all_zero_siblings() {
    let (mut context, operator, instance_pda, operator_pda, mint, user) = setup_release_context();

    let nonce: u64 = 42;
    let all_zero_proofs = [0u8; 512];

    // Generate what the root would be with a valid proof, but use zero proofs
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

/// T010-2: Proof with wrong tree height — provide fewer sibling bytes than expected.
/// The program expects exactly 512 bytes (16 siblings × 32 bytes).
/// We test by providing a valid-looking but incorrect proof from a different nonce.
#[test]
fn test_malformed_proof_wrong_nonce_siblings() {
    let (mut context, operator, instance_pda, operator_pda, mint, user) = setup_release_context();

    let nonce: u64 = 42;
    let wrong_nonce: u64 = 999;

    // Generate proof for wrong_nonce but try to use it for nonce
    let mut smt = ProcessorSMT::new();
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

/// T010-3: Nonce outside current tree_index range.
/// tree_index=0 allows nonces 0..65535. Nonce 65536 should be rejected.
#[test]
fn test_malformed_proof_nonce_outside_tree_range() {
    let (mut context, operator, instance_pda, operator_pda, mint, user) = setup_release_context();

    // Nonce outside tree_index=0 range
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

/// T010-4: Nonce far outside any valid range.
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
