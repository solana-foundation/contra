use crate::{
    smt_utils::ProcessorSMT,
    state_utils::{
        assert_get_or_add_operator, assert_get_or_allow_mint, assert_get_or_create_instance,
        assert_get_or_release_funds, assert_get_or_reset_smt_root,
    },
    utils::{
        assert_program_error, set_mint, setup_test_balances, TestContext, INVALID_SMT_PROOF_ERROR,
        INVALID_TRANSACTION_NONCE_FOR_CURRENT_TREE_INDEX_ERROR,
    },
};

use solana_sdk::signature::{Keypair, Signer};
use spl_token::ID as TOKEN_PROGRAM_ID;

const LARGE_DEPOSIT: u64 = 10_000_000;
const RELEASE_AMOUNT: u64 = 100_000;

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

    assert_get_or_reset_smt_root(&mut context, &operator, &instance_pda, &operator_pda, false)
        .expect("Reset SMT root should succeed");

    // After reset, tree_index=1 expects nonces 65536..131071.
    // Nonce 42 belongs to tree_index=0 and should be rejected.
    context.warp_to_slot(2);

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

#[test]
fn test_double_spend_smt_exclusion_rejects_used_nonce() {
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

    // Replay the same nonce without tree reset. The on-chain root now has nonce 42
    // as a non-empty leaf, so the exclusion proof fails — the SMT math itself
    // prevents double-spend, not just tree_index validation.
    context.warp_to_slot(2);

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
        sibling_proofs,
        false,
    );

    assert_program_error(result, INVALID_SMT_PROOF_ERROR);
}

#[test]
fn test_double_spend_sequential_releases_then_replay() {
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

    let mut smt = ProcessorSMT::new();

    for nonce in [42u64, 43, 44] {
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
        .unwrap_or_else(|_| panic!("Release with nonce {} should succeed", nonce));
    }

    // Replay nonce 42 after three sequential releases.
    // On-chain root has nonces 42, 43, 44 — exclusion proof for 42 fails.
    // Fresh SMT proofs won't match the on-chain root that already contains 42.
    context.warp_to_slot(2);

    let replay_nonce: u64 = 42;

    let mut replay_smt = ProcessorSMT::new();
    let (_, replay_proofs) = replay_smt.generate_exclusion_proof_for_verification(replay_nonce);
    replay_smt.insert(replay_nonce);
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
        replay_nonce,
        replay_proofs,
        false,
    );

    assert_program_error(result, INVALID_SMT_PROOF_ERROR);
}
