use crate::{
    smt_utils::ProcessorSMT,
    state_utils::{
        assert_get_or_add_operator, assert_get_or_allow_mint, assert_get_or_create_instance,
        assert_get_or_release_funds, assert_get_or_reset_smt_root,
    },
    utils::{
        assert_program_error, set_mint, setup_test_balances, TestContext,
        INVALID_TRANSACTION_NONCE_FOR_CURRENT_TREE_INDEX_ERROR,
    },
};

use solana_sdk::signature::{Keypair, Signer};
use spl_token::ID as TOKEN_PROGRAM_ID;

const LARGE_DEPOSIT: u64 = 10_000_000;
const RELEASE_AMOUNT: u64 = 100_000;

/// T009: Double-spend test — replay the exact same nonce after tree reset.
///
/// Steps:
/// 1. Deposit tokens
/// 2. Release with nonce N
/// 3. Reset SMT root (increments tree_index)
/// 4. Attempt to release again with the SAME nonce N
///
/// Expected: Step 4 fails because nonce N belongs to tree_index=0 range,
/// but after reset the instance is on tree_index=1.
#[test]
fn test_double_spend_same_nonce_after_tree_reset() {
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

    // Step 2: Release with nonce N=42
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
        RELEASE_AMOUNT,
        &user.pubkey(),
        new_root,
        nonce,
        sibling_proofs,
        false,
    )
    .expect("First release with nonce 42 should succeed");

    // Step 3: Reset SMT root
    assert_get_or_reset_smt_root(&mut context, &operator, &instance_pda, &operator_pda, false)
        .expect("Reset SMT root should succeed");

    // Step 4: Try to replay the SAME nonce 42 after reset
    // After reset, tree_index=1, so valid nonces are 65536..131071.
    // Nonce 42 belongs to tree_index=0 and should be rejected.
    let mut replay_smt = ProcessorSMT::new();
    let (_, replay_proofs) = replay_smt.generate_exclusion_proof_for_verification(nonce);
    replay_smt.insert(nonce);
    let replay_root = replay_smt.current_root();

    let result = assert_get_or_release_funds(
        &mut context,
        &operator,
        &instance_pda,
        &operator_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
        RELEASE_AMOUNT,
        &user.pubkey(),
        replay_root,
        nonce,
        replay_proofs,
        false,
    );

    assert_program_error(
        result,
        INVALID_TRANSACTION_NONCE_FOR_CURRENT_TREE_INDEX_ERROR,
    );
}
