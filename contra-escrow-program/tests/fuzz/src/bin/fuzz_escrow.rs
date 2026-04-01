//! # Fuzz harness for the contra-escrow program
//!
//! ## What is being fuzzed
//!
//! This harness exercises the core deposit → release flow of the escrow program
//! against a full LiteSVM runtime (the same one used by integration tests).
//! honggfuzz generates and mutates inputs automatically, guided by branch coverage.
//!
//! Each fuzz iteration:
//!   1. Spins up a fresh LiteSVM instance with the compiled program loaded
//!   2. Creates an instance, allows a mint, registers an operator
//!   3. Funds a user token account and deposits into the escrow
//!   4. Attempts a release with either a valid or garbage SMT proof
//!   5. Optionally tries a second release with the same nonce (double-spend)
//!
//! ## Invariants checked
//!
//! - Valid proof + sufficient funds → release MUST succeed and balances MUST
//!   move exactly `release_amount` tokens from the instance ATA to the user ATA
//! - Invalid proof → release MUST be rejected, balances MUST be unchanged
//! - Double-spend (reused nonce) → second release MUST always be rejected,
//!   balances MUST be unchanged after the failed attempt
//!
//! ## How coverage guidance works
//!
//! honggfuzz instruments the host-side Rust code (this harness + LiteSVM +
//! the client) at compile time. It tracks which branches are hit on each run
//! and mutates inputs to explore new branches. The BPF bytecode running inside
//! LiteSVM is NOT instrumented — honggfuzz drives the program indirectly by
//! finding inputs that exercise different program paths via the SVM's responses.
//!
//! ## Running
//!
//! ```sh
//! cd contra-escrow-program/tests/fuzz
//! cargo hfuzz run fuzz_escrow          # run indefinitely
//! cargo hfuzz run fuzz_escrow -- -n 10000  # stop after 10k iterations
//! ```

use arbitrary::Arbitrary;
use contra_escrow_program_client::instructions::ReleaseFundsBuilder;
use honggfuzz::fuzz;
use solana_sdk::signature::{Keypair, Signer};
use spl_associated_token_account::get_associated_token_address_with_program_id;
use spl_token::ID as TOKEN_PROGRAM_ID;
use tests_contra_escrow_program::{
    pda_utils::{find_allowed_mint_pda, find_event_authority_pda},
    smt_utils::ProcessorSMT,
    state_utils::{
        assert_get_or_add_operator, assert_get_or_allow_mint, assert_get_or_create_instance,
        assert_get_or_deposit,
    },
    utils::{
        get_token_balance, set_mint, set_token_balance, ATA_PROGRAM_ID, CONTRA_ESCROW_PROGRAM_ID,
        TestContext,
    },
};

/// Structured input derived from raw fuzzer bytes via the `Arbitrary` trait.
/// honggfuzz mutates the underlying byte slice; `Arbitrary` maps it to typed
/// fields so every generated value is structurally valid Rust — no hand-written
/// parsing needed.
#[derive(Arbitrary, Debug)]
struct FuzzInput {
    /// How many tokens the user deposits into the escrow
    deposit_amount: u64,
    /// How many tokens the operator tries to release back to the user
    release_amount: u64,
    /// Withdrawal nonce passed to the SMT proof and the program
    transaction_nonce: u64,
    /// When true: generate a cryptographically valid SMT exclusion proof.
    /// When false: pass garbage bytes — the program must reject them.
    use_valid_proof: bool,
    /// When true: after a successful release, attempt a second release with
    /// the exact same nonce to test double-spend protection.
    try_double_spend: bool,
}

fn run_fuzz(input: FuzzInput) {
    // Clamp to sane ranges to avoid trivial edge cases and keep the SMT tree
    // small (smaller nonce → fewer leaves traversed → faster iteration).
    let deposit_amount = (input.deposit_amount % 1_000_000).max(1);
    let release_amount = input.release_amount % (deposit_amount + 1);
    // Keeping nonce below 1_000 avoids building a full 65k-leaf tree on every
    // run, which was causing ~14 timeouts per 800 iterations.
    let nonce = input.transaction_nonce % 1_000;

    // Fresh SVM state for every iteration — no shared state between runs.
    let mut context = TestContext::new();
    let admin = Keypair::new();
    let operator = Keypair::new();
    let user = Keypair::new();
    let mint_keypair = Keypair::new();
    let mint = mint_keypair.pubkey();

    // --- Environment setup ---
    // These operations are expected to succeed; panicking here would indicate
    // a bug in the test setup, not the program under test.

    let instance_seed = Keypair::new();
    let (instance_pda, _) =
        assert_get_or_create_instance(&mut context, &admin, &instance_seed, true, false)
            .expect("create_instance failed");

    // Register a standard SPL-token mint (not Token-2022) with fixed supply.
    set_mint(&mut context, &mint);

    assert_get_or_allow_mint(&mut context, &admin, &instance_pda, &mint, true, false)
        .expect("allow_mint failed");

    let (operator_pda, _) =
        assert_get_or_add_operator(&mut context, &admin, &instance_pda, &operator.pubkey(), true, false)
            .expect("add_operator failed");

    // Directly set the user's token balance in LiteSVM state — no mint
    // authority needed, avoids extra transactions.
    let user_ata =
        get_associated_token_address_with_program_id(&user.pubkey(), &mint, &TOKEN_PROGRAM_ID);
    set_token_balance(&mut context, &user_ata, &mint, &user.pubkey(), deposit_amount);

    // --- Deposit ---

    assert_get_or_deposit(
        &mut context,
        &user,
        &instance_pda,
        &mint,
        &TOKEN_PROGRAM_ID,
        deposit_amount,
        None,
        false,
    )
    .expect("deposit failed");

    let instance_ata =
        get_associated_token_address_with_program_id(&instance_pda, &mint, &TOKEN_PROGRAM_ID);

    // --- Build SMT proof ---

    let mut smt = ProcessorSMT::new();

    let (sibling_proofs, new_withdrawal_root) = if input.use_valid_proof {
        // Generate a real exclusion proof proving `nonce` is not yet in the
        // tree, then compute the new root after marking it as used.
        let (_, proofs) = smt.generate_exclusion_proof(nonce);
        smt.insert(nonce);
        let new_root = smt.current_root();
        (proofs, new_root)
    } else {
        // Deliberately invalid: all 0xdd bytes for proofs, all 0xff for root.
        // The program must detect the mismatch and return an error.
        ([0xddu8; 512], [0xffu8; 32])
    };

    // --- Attempt release ---

    let (allowed_mint_pda, _) = find_allowed_mint_pda(&instance_pda, &mint);
    let (event_authority_pda, _) = find_event_authority_pda();

    let instance_balance_before = get_token_balance(&mut context, &instance_ata);
    let user_balance_before = get_token_balance(&mut context, &user_ata);

    let release_ix = ReleaseFundsBuilder::new()
        .payer(context.payer.pubkey())
        .operator(operator.pubkey())
        .instance(instance_pda)
        .operator_pda(operator_pda)
        .mint(mint)
        .allowed_mint(allowed_mint_pda)
        .user_ata(user_ata)
        .instance_ata(instance_ata)
        .token_program(TOKEN_PROGRAM_ID)
        .associated_token_program(ATA_PROGRAM_ID)
        .event_authority(event_authority_pda)
        .contra_escrow_program(CONTRA_ESCROW_PROGRAM_ID)
        .amount(release_amount)
        .user(user.pubkey())
        .new_withdrawal_root(new_withdrawal_root)
        .transaction_nonce(nonce)
        .sibling_proofs(sibling_proofs)
        .instruction();

    let release_result = context.send_transaction_with_signers(release_ix, &[&operator]);

    if input.use_valid_proof && release_amount <= instance_balance_before {
        // Valid proof + sufficient funds: the program MUST accept this.
        release_result.expect("valid release should succeed");

        // Invariant: tokens moved exactly as expected.
        let instance_balance_after = get_token_balance(&mut context, &instance_ata);
        let user_balance_after = get_token_balance(&mut context, &user_ata);

        assert_eq!(
            instance_balance_after,
            instance_balance_before - release_amount,
            "instance ATA balance mismatch after release"
        );
        assert_eq!(
            user_balance_after,
            user_balance_before + release_amount,
            "user ATA balance mismatch after release"
        );

        // --- Double-spend attempt ---
        if input.try_double_spend {
            // Replay the exact same instruction: same nonce, same proofs, same
            // root. The nonce is now included in the on-chain SMT, so the
            // program must reject this as a double-spend.
            let double_spend_ix = ReleaseFundsBuilder::new()
                .payer(context.payer.pubkey())
                .operator(operator.pubkey())
                .instance(instance_pda)
                .operator_pda(operator_pda)
                .mint(mint)
                .allowed_mint(allowed_mint_pda)
                .user_ata(user_ata)
                .instance_ata(instance_ata)
                .token_program(TOKEN_PROGRAM_ID)
                .associated_token_program(ATA_PROGRAM_ID)
                .event_authority(event_authority_pda)
                .contra_escrow_program(CONTRA_ESCROW_PROGRAM_ID)
                .amount(release_amount)
                .user(user.pubkey())
                .new_withdrawal_root(new_withdrawal_root)
                .transaction_nonce(nonce)
                .sibling_proofs(sibling_proofs)
                .instruction();

            let double_result =
                context.send_transaction_with_signers(double_spend_ix, &[&operator]);

            // Invariant: double-spend must always be rejected.
            assert!(
                double_result.is_err(),
                "double-spend with reused nonce must be rejected"
            );

            // Invariant: balances must be unchanged after the failed attempt.
            let instance_balance_final = get_token_balance(&mut context, &instance_ata);
            let user_balance_final = get_token_balance(&mut context, &user_ata);
            assert_eq!(
                instance_balance_final, instance_balance_after,
                "instance ATA changed after double-spend"
            );
            assert_eq!(
                user_balance_final, user_balance_after,
                "user ATA changed after double-spend"
            );
        }
    } else {
        // Invalid proof or release_amount > deposited: program MUST reject.
        assert!(
            release_result.is_err(),
            "release with invalid proof or excess amount must fail"
        );

        // Invariant: no tokens must have moved despite the failed transaction.
        let instance_balance_after = get_token_balance(&mut context, &instance_ata);
        let user_balance_after = get_token_balance(&mut context, &user_ata);
        assert_eq!(
            instance_balance_after, instance_balance_before,
            "instance ATA changed despite failed release"
        );
        assert_eq!(
            user_balance_after, user_balance_before,
            "user ATA changed despite failed release"
        );
    }
}

fn main() {
    loop {
        fuzz!(|data: &[u8]| {
            let mut u = arbitrary::Unstructured::new(data);
            if let Ok(input) = FuzzInput::arbitrary(&mut u) {
                run_fuzz(input);
            }
        });
    }
}
