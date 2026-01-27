use crate::{
    pda_utils::{find_allowed_mint_pda, find_event_authority_pda, find_operator_pda},
    smt_utils::{ProcessorSMT, MAX_TREE_LEAVES},
    state_utils::{
        assert_get_or_add_operator, assert_get_or_allow_mint, assert_get_or_create_instance,
        assert_get_or_deposit, assert_get_or_release_funds, assert_get_or_reset_smt_root,
    },
    utils::{
        assert_program_error, set_mint, setup_test_balances, TestContext, ATA_PROGRAM_ID,
        CONTRA_ESCROW_PROGRAM_ID, INVALID_INSTRUCTION_DATA_ERROR, INVALID_OPERATOR_ERROR,
        INVALID_SMT_PROOF_ERROR, INVALID_TRANSACTION_NONCE_FOR_CURRENT_TREE_INDEX_ERROR,
        MISSING_REQUIRED_SIGNATURE_ERROR, TOKEN_INSUFFICIENT_FUNDS_ERROR,
    },
};

use contra_escrow_program_client::instructions::ReleaseFundsBuilder;
use contra_escrow_program_client::Instance;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    signature::{Keypair, Signer},
};
use spl_associated_token_account::get_associated_token_address_with_program_id;
use spl_token::ID as TOKEN_PROGRAM_ID;

const DEPOSIT_AMOUNT: u64 = 1_000_000; // 1 token with 6 decimals
const RELEASE_AMOUNT: u64 = 500_000; // 0.5 tokens with 6 decimals
const TRANSACTION_NONCE: u64 = 42; // Transaction nonce for SMT exclusion

#[test]
fn test_release_funds_success() {
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

    // Add operator
    let (operator_pda, _) = assert_get_or_add_operator(
        &mut context,
        &admin,
        &instance_pda,
        &operator.pubkey(),
        false,
        false,
    )
    .expect("AddOperator should succeed");

    // Setup and perform deposit
    setup_test_balances(
        &mut context,
        &user,
        &instance_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
        DEPOSIT_AMOUNT,
        RELEASE_AMOUNT,
    );

    assert_get_or_deposit(
        &mut context,
        &user,
        &instance_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
        DEPOSIT_AMOUNT,
        None,
        false,
    )
    .expect("Deposit should succeed");

    let mut smt = ProcessorSMT::new();
    let (_, sibling_proofs) = smt.generate_exclusion_proof_for_verification(TRANSACTION_NONCE);

    // Calculate the new root after adding the transaction nonce
    smt.insert(TRANSACTION_NONCE);
    let new_withdrawal_root = smt.current_root();

    // Release funds using utility function
    assert_get_or_release_funds(
        &mut context,
        &operator,
        &instance_pda,
        &operator_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
        RELEASE_AMOUNT,
        &user.pubkey(),
        new_withdrawal_root,
        TRANSACTION_NONCE,
        sibling_proofs,
        true,
    )
    .expect("ReleaseFunds should succeed");
}

#[test]
fn test_release_funds_insufficient_funds() {
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

    // Add operator
    let (operator_pda, _) = assert_get_or_add_operator(
        &mut context,
        &admin,
        &instance_pda,
        &operator.pubkey(),
        false,
        true,
    )
    .expect("AddOperator should succeed");

    // Setup deposit test but don't perform deposit - this means the instance ATA will have 0 balance
    setup_test_balances(
        &mut context,
        &user,
        &instance_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
        DEPOSIT_AMOUNT,
        0, // Set instance balance to 0 to create insufficient funds scenario
    );

    let mut smt = ProcessorSMT::new();
    let (_, sibling_proofs) = smt.generate_exclusion_proof_for_verification(TRANSACTION_NONCE);

    // Calculate the new root after adding the transaction nonce
    smt.insert(TRANSACTION_NONCE);
    let new_withdrawal_root = smt.current_root();

    let result = assert_get_or_release_funds(
        &mut context,
        &operator,
        &instance_pda,
        &operator_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
        RELEASE_AMOUNT,
        &user.pubkey(),
        new_withdrawal_root,
        TRANSACTION_NONCE,
        sibling_proofs,
        false,
    );

    assert_program_error(result, TOKEN_INSUFFICIENT_FUNDS_ERROR);
}

#[test]
fn test_release_funds_not_operator() {
    let mut context = TestContext::new();
    let admin = Keypair::new();
    let operator = Keypair::new();
    let fake_operator = Keypair::new(); // Not added as operator
    let user = Keypair::new();
    let mint = Keypair::new();

    let instance_seed = Keypair::new();

    set_mint(&mut context, &mint.pubkey());

    // Another instance for fake operator
    let instance_seed_2 = Keypair::new();
    let (instance_pda_2, _) =
        assert_get_or_create_instance(&mut context, &admin, &instance_seed_2, false, false)
            .expect("CreateInstance should succeed");
    assert_get_or_add_operator(
        &mut context,
        &admin,
        &instance_pda_2,
        &fake_operator.pubkey(),
        false,
        false,
    )
    .expect("AddOperator should succeed");

    // Real valid instance
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

    // Add legitimate operator
    assert_get_or_add_operator(
        &mut context,
        &admin,
        &instance_pda,
        &operator.pubkey(),
        false,
        false,
    )
    .expect("AddOperator should succeed");

    // Setup and perform deposit
    setup_test_balances(
        &mut context,
        &user,
        &instance_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
        DEPOSIT_AMOUNT,
        RELEASE_AMOUNT,
    );

    assert_get_or_deposit(
        &mut context,
        &user,
        &instance_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
        DEPOSIT_AMOUNT,
        None,
        false,
    )
    .expect("Deposit should succeed");

    // Try to release funds with fake operator
    context
        .airdrop_if_required(&fake_operator.pubkey(), 1_000_000_000)
        .unwrap();

    let (allowed_mint_pda, _) = find_allowed_mint_pda(&instance_pda, &mint.pubkey());
    let (event_authority_pda, _) = find_event_authority_pda();
    let (fake_operator_pda, _) = find_operator_pda(&instance_pda_2, &fake_operator.pubkey());

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

    let mut smt = ProcessorSMT::new();
    let (_, sibling_proofs) = smt.generate_exclusion_proof_for_verification(TRANSACTION_NONCE);

    // Calculate the new root after adding the transaction nonce
    smt.insert(TRANSACTION_NONCE);
    let new_withdrawal_root = smt.current_root();

    let instruction = ReleaseFundsBuilder::new()
        .payer(context.payer.pubkey())
        .operator(fake_operator.pubkey())
        .instance(instance_pda)
        .operator_pda(fake_operator_pda)
        .mint(mint.pubkey())
        .allowed_mint(allowed_mint_pda)
        .user_ata(user_ata)
        .instance_ata(instance_ata)
        .token_program(TOKEN_PROGRAM_ID)
        .associated_token_program(ATA_PROGRAM_ID)
        .event_authority(event_authority_pda)
        .contra_escrow_program(CONTRA_ESCROW_PROGRAM_ID)
        .amount(RELEASE_AMOUNT)
        .user(user.pubkey())
        .new_withdrawal_root(new_withdrawal_root)
        .transaction_nonce(TRANSACTION_NONCE)
        .sibling_proofs(sibling_proofs)
        .instruction();

    let result = context.send_transaction_with_signers(instruction, &[&fake_operator]);

    assert_program_error(result, INVALID_OPERATOR_ERROR);
}

#[test]
fn test_release_funds_invalid_instruction_data_too_short() {
    let mut context = TestContext::new();

    let instruction = Instruction {
        program_id: CONTRA_ESCROW_PROGRAM_ID,
        accounts: vec![],
        data: vec![7, 1, 2, 3], // Too short instruction data (discriminator + partial data)
    };

    let result = context.send_transaction(instruction);
    assert_program_error(result, INVALID_INSTRUCTION_DATA_ERROR);
}

#[test]
fn test_release_funds_operator_not_signer() {
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

    // Add operator
    let (operator_pda, _) = assert_get_or_add_operator(
        &mut context,
        &admin,
        &instance_pda,
        &operator.pubkey(),
        false,
        false,
    )
    .expect("AddOperator should succeed");

    // Setup and perform deposit
    setup_test_balances(
        &mut context,
        &user,
        &instance_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
        DEPOSIT_AMOUNT,
        RELEASE_AMOUNT,
    );

    assert_get_or_deposit(
        &mut context,
        &user,
        &instance_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
        DEPOSIT_AMOUNT,
        None,
        false,
    )
    .expect("Deposit should succeed");

    let mut smt = ProcessorSMT::new();
    let (_, sibling_proofs) = smt.generate_exclusion_proof_for_verification(TRANSACTION_NONCE);

    // Calculate the new root after adding the transaction nonce
    smt.insert(TRANSACTION_NONCE);
    let new_withdrawal_root = smt.current_root();

    // Try to release funds with operator not marked as signer
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

    // Create instruction where operator is NOT marked as signer (12 accounts)
    let accounts = vec![
        AccountMeta::new(context.payer.pubkey(), true), // payer (signer, writable)
        AccountMeta::new_readonly(operator.pubkey(), false), // operator (NOT signer)
        AccountMeta::new(instance_pda, false),          // instance (writable)
        AccountMeta::new_readonly(operator_pda, false), // operator_pda
        AccountMeta::new_readonly(mint.pubkey(), false), // mint
        AccountMeta::new_readonly(allowed_mint_pda, false), // allowed_mint
        AccountMeta::new(user_ata, false),              // user_ata (writable)
        AccountMeta::new(instance_ata, false),          // instance_ata (writable)
        AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false), // token_program
        AccountMeta::new_readonly(ATA_PROGRAM_ID, false), // associated_token_program
        AccountMeta::new_readonly(event_authority_pda, false), // event_authority
        AccountMeta::new_readonly(CONTRA_ESCROW_PROGRAM_ID, false), // contra_escrow_program
    ];

    let mut data = vec![7]; // discriminator for ReleaseFunds
    data.extend_from_slice(&RELEASE_AMOUNT.to_le_bytes()); // amount (8 bytes)
    data.extend_from_slice(user.pubkey().as_ref()); // user (32 bytes)
    data.extend_from_slice(&new_withdrawal_root); // new_withdrawal_root (32 bytes)
    data.extend_from_slice(&TRANSACTION_NONCE.to_le_bytes()); // transaction_nonce (8 bytes)
    data.extend_from_slice(&sibling_proofs); // sibling_proofs (512 bytes)

    let instruction = Instruction {
        program_id: CONTRA_ESCROW_PROGRAM_ID,
        accounts,
        data,
    };

    let result = context.send_transaction_with_signers(instruction, &[]);

    assert_program_error(result, MISSING_REQUIRED_SIGNATURE_ERROR);
}

#[test]
fn test_release_funds_smt_exclusion() {
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

    // Add operator
    let (operator_pda, _) = assert_get_or_add_operator(
        &mut context,
        &admin,
        &instance_pda,
        &operator.pubkey(),
        false,
        false,
    )
    .expect("AddOperator should succeed");

    // Setup test with a large deposit to support multiple releases
    let large_deposit = 10_000_000; // 10 tokens with 6 decimals
    let release_amount = 100_000; // 0.1 tokens per release

    setup_test_balances(
        &mut context,
        &user,
        &instance_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
        0,
        large_deposit, // Give escrow full amount
    );

    let mut instance_smt = ProcessorSMT::new();
    let mut used_nonces = std::collections::HashSet::new();

    // Test scenarios: mix of valid and duplicate nonces
    let test_nonces = [
        1, 2, 3, 5, 8, 13, 21, 34, 55, 89, // Valid unique nonces
        144, 233, 377, 610, 987, 1597, // More valid nonces
        1, 2, 3, // Duplicates (should fail)
        999, 1000, 1001, 1002, // More unique valid nonces
        5, 8, // More duplicates (should fail)
        2000, 2001, 2002, 2003, 2004, // Final batch of unique nonces
    ];

    for &nonce in test_nonces.iter() {
        let is_duplicate = used_nonces.contains(&nonce);

        if is_duplicate {
            // For duplicates, nonce already exists in our SMT - exclusion proof should fail

            // Use current SMT root and generate fake proof (won't work anyway)
            let current_root = instance_smt.current_root();
            let fake_sibling_proofs = [0u8; 512]; // Invalid proof

            let result = assert_get_or_release_funds(
                &mut context,
                &operator,
                &instance_pda,
                &operator_pda,
                &mint.pubkey(),
                &TOKEN_PROGRAM_ID,
                release_amount,
                &user.pubkey(),
                current_root, // Same root since we're not adding anything
                nonce,
                fake_sibling_proofs,
                false,
            );

            assert_program_error(result, INVALID_SMT_PROOF_ERROR);
        } else {
            // For new nonces, generate valid exclusion proof against current SMT state
            let (_, sibling_proofs) = instance_smt.generate_exclusion_proof_for_verification(nonce);

            // Calculate what the new root will be after adding this nonce
            let mut new_smt = instance_smt.clone();
            new_smt.insert(nonce);
            let new_withdrawal_root = new_smt.current_root();

            let result = assert_get_or_release_funds(
                &mut context,
                &operator,
                &instance_pda,
                &operator_pda,
                &mint.pubkey(),
                &TOKEN_PROGRAM_ID,
                release_amount,
                &user.pubkey(),
                new_withdrawal_root,
                nonce,
                sibling_proofs,
                false,
            );

            assert!(result.is_ok(), "New nonce {} should succeed", nonce);

            // Success: Update our SMT to mirror the instance's new state
            used_nonces.insert(nonce);
            instance_smt.insert(nonce);
        }
    }
}

#[test]
fn test_release_funds_invalid_inclusion_proof() {
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
        DEPOSIT_AMOUNT,
        RELEASE_AMOUNT,
    );

    assert_get_or_deposit(
        &mut context,
        &user,
        &instance_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
        DEPOSIT_AMOUNT,
        None,
        false,
    )
    .expect("Deposit should succeed");

    // Generate valid exclusion proof from empty SMT
    let smt = ProcessorSMT::new();
    let (_, sibling_proofs) = smt.generate_exclusion_proof_for_verification(TRANSACTION_NONCE);

    // Provide WRONG new root - this will pass exclusion but fail inclusion proof
    // The exclusion proof will be valid against empty tree, but inclusion proof
    // will fail because wrong_new_root doesn't match what the tree would look like
    // after adding the nonce
    let wrong_new_root = [42u8; 32]; // Arbitrary wrong root

    let result = assert_get_or_release_funds(
        &mut context,
        &operator,
        &instance_pda,
        &operator_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
        RELEASE_AMOUNT,
        &user.pubkey(),
        wrong_new_root,
        TRANSACTION_NONCE,
        sibling_proofs,
        false,
    );

    assert_program_error(result, INVALID_SMT_PROOF_ERROR);
}

#[test]
fn test_release_funds_with_smt_reset() {
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

    // Setup balances for multiple releases (large deposit to support multiple releases)
    let large_deposit = 10_000_000; // 10 tokens with 6 decimals
    let release_amount = 100_000; // 0.1 tokens per release

    setup_test_balances(
        &mut context,
        &user,
        &instance_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
        0,             // User doesn't need initial balance for this test
        large_deposit, // Give escrow the full amount
    );

    // Verify initial tree index is 0
    let instance_data = context
        .get_account(&instance_pda)
        .expect("Instance account should exist")
        .data
        .clone();
    let instance = Instance::from_bytes(&instance_data).expect("Should deserialize instance");

    assert_eq!(
        instance.current_tree_index, 0,
        "Initial tree index should be 0"
    );

    // === FIRST RELEASE (Tree Index = 0) ===
    let first_nonce = 42u64; // Nonce in range 0-65535 for tree_index=0

    let mut first_smt = ProcessorSMT::new();
    let (_, first_sibling_proofs) =
        first_smt.generate_exclusion_proof_for_verification(first_nonce);

    // Calculate new root after adding the nonce
    first_smt.insert(first_nonce);
    let first_new_root = first_smt.current_root();

    // First release should succeed
    assert_get_or_release_funds(
        &mut context,
        &operator,
        &instance_pda,
        &operator_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
        release_amount,
        &user.pubkey(),
        first_new_root,
        first_nonce,
        first_sibling_proofs,
        false,
    )
    .expect("First release should succeed");

    // === RESET SMT ROOT ===
    assert_get_or_reset_smt_root(&mut context, &operator, &instance_pda, &operator_pda, false)
        .expect("Reset SMT root should succeed");

    // Verify tree index incremented to 1
    let instance_data = context
        .get_account(&instance_pda)
        .expect("Instance account should exist")
        .data
        .clone();
    let instance = Instance::from_bytes(&instance_data).expect("Should deserialize instance");

    assert_eq!(
        instance.current_tree_index, 1,
        "Tree index should be 1 after reset"
    );

    // === SECOND RELEASE (Tree Index = 1) ===
    let second_nonce = 65536u64; // First nonce for tree_index=1 (65536 / 65536 = 1)

    let mut second_smt = ProcessorSMT::new(); // Fresh SMT after reset
    let (_, second_sibling_proofs) =
        second_smt.generate_exclusion_proof_for_verification(second_nonce);

    // Calculate new root after adding the nonce
    second_smt.insert(second_nonce);
    let second_new_root = second_smt.current_root();

    // Second release with correct nonce range should succeed
    assert_get_or_release_funds(
        &mut context,
        &operator,
        &instance_pda,
        &operator_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
        release_amount,
        &user.pubkey(),
        second_new_root,
        second_nonce,
        second_sibling_proofs,
        false,
    )
    .expect("Second release with nonce 65536 should succeed");

    // === NEGATIVE TEST: Try old nonce after reset (should fail) ===
    let old_range_nonce = 123u64; // Different nonce in range 0-65535 (tree_index=0)
    let mut old_nonce_smt = ProcessorSMT::new();
    let (_, old_sibling_proofs) =
        old_nonce_smt.generate_exclusion_proof_for_verification(old_range_nonce);
    old_nonce_smt.insert(old_range_nonce);
    let old_new_root = old_nonce_smt.current_root();

    // Try to use nonce in old range (123) after reset - should fail due to tree index mismatch
    let result = assert_get_or_release_funds(
        &mut context,
        &operator,
        &instance_pda,
        &operator_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
        release_amount,
        &user.pubkey(),
        old_new_root,
        old_range_nonce, // This is now invalid for tree_index=1
        old_sibling_proofs,
        false,
    );

    // Should fail with specific error for invalid transaction nonce for current tree index
    assert_program_error(
        result,
        INVALID_TRANSACTION_NONCE_FOR_CURRENT_TREE_INDEX_ERROR,
    );

    // === NEGATIVE TEST: Try wrong range nonce (should fail) ===
    let wrong_nonce = MAX_TREE_LEAVES as u64 * 10;
    let mut wrong_smt = ProcessorSMT::new();
    let (_, wrong_sibling_proofs) =
        wrong_smt.generate_exclusion_proof_for_verification(wrong_nonce);
    wrong_smt.insert(wrong_nonce);
    let wrong_new_root = wrong_smt.current_root();

    let result = assert_get_or_release_funds(
        &mut context,
        &operator,
        &instance_pda,
        &operator_pda,
        &mint.pubkey(),
        &TOKEN_PROGRAM_ID,
        release_amount,
        &user.pubkey(),
        wrong_new_root,
        wrong_nonce,
        wrong_sibling_proofs,
        false,
    );

    assert_program_error(
        result,
        INVALID_TRANSACTION_NONCE_FOR_CURRENT_TREE_INDEX_ERROR,
    );
}
