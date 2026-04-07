//! # Fuzz harness for the contra-escrow program
//!
//! ## What this tests
//!
//! Each fuzz input is a sequence of escrow operations executed against a single
//! LiteSVM instance. The harness drives the on-chain program and checks two
//! classes of invariants after every operation:
//!
//! - **Per-operation**: token balances shift by exactly the expected amount (or
//!   not at all for failed operations).
//! - **End-to-end**: `final_instance_balance == total_deposited - total_released`
//!   and the user balance mirrors the inverse.
//!
//! ## Operations
//!
//! | `Op`          | What it does                                                        |
//! |---------------|---------------------------------------------------------------------|
//! | `Deposit`     | User deposits tokens into the escrow ATA.                           |
//! | `Release`     | Operator releases funds to the user, protected by an SMT proof.     |
//! | `DoubleSpend` | Replays a successful release; must always be rejected by the program.|
//!
//! ## SMT proof system (quick primer)
//!
//! The program keeps a Sparse Merkle Tree (SMT) root on-chain. To release funds
//! the operator must supply:
//!
//! 1. An **exclusion proof**: sibling hashes that prove `nonce` is NOT in the
//!    current tree (leaf starts as `EMPTY_LEAF`).
//! 2. A **new root**: the tree root after marking `nonce` as used.
//! 3. An **inclusion proof**: the same sibling hashes prove `nonce` IS in the
//!    new tree (leaf starts as `NON_EMPTY_LEAF_HASH`).
//!
//! After a successful release the on-chain root is replaced with the new root,
//! making any future exclusion proof for the same nonce invalid. This is the
//! core anti-double-spend mechanism that this harness stress-tests.

use arbitrary::Arbitrary;
use contra_escrow_fuzz::{
    build_release_ix, clamp_amount, clamp_nonce, setup_fuzz_context, FuzzContext, SuccessfulRelease,
};
use honggfuzz::fuzz;
use spl_associated_token_account::get_associated_token_address_with_program_id;
use spl_token::ID as TOKEN_PROGRAM_ID;
use tests_contra_escrow_program::{state_utils::assert_get_or_deposit, utils::get_token_balance};

// ── Fuzz input ───────────────────────────────────────────────────────────────

/// A single step in the fuzz sequence.
#[derive(Arbitrary, Debug, Clone)]
enum Op {
    /// Deposit `amount` tokens from the user into the escrow.
    Deposit { amount: u64 },

    /// Attempt a fund release.
    ///
    /// When `use_valid_proof` is true **and** the nonce hasn't been used yet
    /// **and** the escrow holds enough balance, the harness generates a
    /// cryptographically correct SMT proof and expects the transaction to
    /// succeed. Otherwise it sends garbage data and expects failure.
    Release {
        amount: u64,
        nonce: u64,
        use_valid_proof: bool,
    },

    /// Replay a previously successful release verbatim (same proof, same root,
    /// same amount). The program must reject this because the nonce is already
    /// recorded in the on-chain SMT root.
    DoubleSpend { nonce: u64 },
}

/// The full fuzz input: an ordered sequence of operations.
#[derive(Arbitrary, Debug)]
struct FuzzInput {
    ops: Vec<Op>,
}

// ── Core fuzz logic ───────────────────────────────────────────────────────────

fn run_fuzz(input: FuzzInput, fuzz_context: &mut FuzzContext) {
    let FuzzContext {
        test_context: context,
        operator,
        user,
        mint,
        instance_pda,
        operator_pda,
        user_ata,
        smt,
        successful_releases,
    } = fuzz_context;

    let instance_ata =
        get_associated_token_address_with_program_id(instance_pda, mint, &TOKEN_PROGRAM_ID);

    let initial_user_balance = get_token_balance(context, user_ata);

    // Running totals used for the final balance invariant check.
    let mut total_deposited: u64 = 0;
    let mut total_successful_released: u64 = 0;

    for (slot, op) in input.ops.into_iter().take(32).enumerate() {
        // Advance the slot before every operation so repeated transactions with
        // identical parameters get a fresh blockhash and unique signature.
        context.warp_to_slot(slot as u64 + 2);

        match op {
            // ── Deposit ───────────────────────────────────────────────────────
            Op::Deposit { amount } => {
                let deposit_amount = clamp_amount(amount);

                let instance_balance_before = get_token_balance(context, &instance_ata);
                let user_balance_before = get_token_balance(context, user_ata);

                assert_get_or_deposit(
                    context,
                    user,
                    instance_pda,
                    mint,
                    &TOKEN_PROGRAM_ID,
                    deposit_amount,
                    None,
                    false,
                )
                .expect("deposit failed");

                // Tokens must move from user to instance — no more, no less.
                assert_eq!(
                    get_token_balance(context, &instance_ata),
                    instance_balance_before + deposit_amount,
                    "instance ATA did not increase by deposit amount"
                );
                assert_eq!(
                    get_token_balance(context, user_ata),
                    user_balance_before - deposit_amount,
                    "user ATA did not decrease by deposit amount"
                );

                total_deposited = total_deposited
                    .checked_add(deposit_amount)
                    .expect("total_deposited overflow");
            }

            // ── Release ───────────────────────────────────────────────────────
            Op::Release {
                amount,
                nonce,
                use_valid_proof,
            } => {
                let release_amount = clamp_amount(amount);
                let nonce = clamp_nonce(nonce);

                let instance_balance_before = get_token_balance(context, &instance_ata);
                let user_balance_before = get_token_balance(context, user_ata);
                let nonce_already_used = smt.contains(nonce);

                // Decide whether this attempt should succeed on-chain and, if
                // so, produce a valid proof. If any precondition fails we fall
                // back to garbage data and expect the program to reject it.
                let (sibling_proofs, new_withdrawal_root, should_succeed) = if use_valid_proof
                    && !nonce_already_used
                    && release_amount <= instance_balance_before
                {
                    // Generate an exclusion proof for `nonce` against the
                    // current root, then compute what the new root will be
                    // after marking `nonce` as used.
                    let (_, proofs) = smt.generate_exclusion_proof(nonce);
                    let mut next_smt = smt.clone();
                    next_smt.insert(nonce);
                    let new_root = next_smt.current_root();

                    (proofs, new_root, true)
                } else {
                    // Intentionally invalid data — the program must reject this.
                    ([0xddu8; 512], [0xffu8; 32], false)
                };

                let release_ix = build_release_ix(
                    context,
                    operator,
                    user,
                    *mint,
                    *instance_pda,
                    *operator_pda,
                    *user_ata,
                    instance_ata,
                    release_amount,
                    nonce,
                    new_withdrawal_root,
                    sibling_proofs,
                );

                let release_result = context
                    .send_transaction_with_signers_with_transaction_result(
                        release_ix,
                        &[&*operator],
                        false,
                        Some(1_200_000),
                    )
                    .map(|_| ());

                if should_succeed {
                    // ── Happy path ─────────────────────────────────────────
                    if let Err(e) = &release_result {
                        panic!(
                            "release should succeed: nonce={} amount={} err={e:?}",
                            nonce, release_amount
                        );
                    }

                    // Update the local SMT mirror to stay in sync with the
                    // on-chain root.
                    smt.insert(nonce);
                    successful_releases.insert(
                        nonce,
                        SuccessfulRelease {
                            amount: release_amount,
                            new_withdrawal_root,
                            sibling_proofs,
                        },
                    );

                    // Tokens must move from instance to user — no more, no less.
                    assert_eq!(
                        get_token_balance(context, &instance_ata),
                        instance_balance_before - release_amount,
                        "instance ATA balance mismatch after release"
                    );
                    assert_eq!(
                        get_token_balance(context, user_ata),
                        user_balance_before + release_amount,
                        "user ATA balance mismatch after release"
                    );

                    total_successful_released = total_successful_released
                        .checked_add(release_amount)
                        .expect("total_successful_released overflow");
                } else {
                    // ── Expected failure path ──────────────────────────────
                    assert!(
                        release_result.is_err(),
                        "release should have failed: nonce={} amount={}",
                        nonce,
                        release_amount
                    );

                    // No tokens should have moved at all.
                    assert_eq!(
                        get_token_balance(context, &instance_ata),
                        instance_balance_before,
                        "instance ATA changed despite failed release"
                    );
                    assert_eq!(
                        get_token_balance(context, user_ata),
                        user_balance_before,
                        "user ATA changed despite failed release"
                    );
                }
            }

            // ── Double Spend ──────────────────────────────────────────────────
            //
            // Replays a previously accepted release verbatim. The on-chain SMT
            // root has already been updated to include `nonce`, so the exclusion
            // proof will no longer verify — the program must reject the replay.
            Op::DoubleSpend { nonce } => {
                let nonce = clamp_nonce(nonce);

                // Skip if this nonce was never successfully released; there is
                // nothing to replay.
                let Some(previous) = successful_releases.get(&nonce).cloned() else {
                    continue;
                };

                let instance_balance_before = get_token_balance(context, &instance_ata);
                let user_balance_before = get_token_balance(context, user_ata);

                let double_spend_ix = build_release_ix(
                    context,
                    operator,
                    user,
                    *mint,
                    *instance_pda,
                    *operator_pda,
                    *user_ata,
                    instance_ata,
                    previous.amount,
                    nonce,
                    previous.new_withdrawal_root,
                    previous.sibling_proofs,
                );

                let result = context
                    .send_transaction_with_signers_with_transaction_result(
                        double_spend_ix,
                        &[&*operator],
                        false,
                        Some(1_200_000),
                    )
                    .map(|_| ());

                assert!(
                    result.is_err(),
                    "double-spend replay must fail: nonce={}",
                    nonce
                );

                // No tokens should have moved.
                assert_eq!(
                    get_token_balance(context, &instance_ata),
                    instance_balance_before,
                    "instance ATA changed after double-spend replay"
                );
                assert_eq!(
                    get_token_balance(context, user_ata),
                    user_balance_before,
                    "user ATA changed after double-spend replay"
                );
            }
        }
    }

    // ── Final balance invariant ───────────────────────────────────────────────
    //
    // After all operations: everything deposited must either still be in the
    // escrow or have been released to the user — nothing can disappear or appear
    // from nowhere.
    let expected_instance_balance = total_deposited
        .checked_sub(total_successful_released)
        .expect("released more than deposited — model invariant violated");

    let expected_user_balance = initial_user_balance
        .checked_sub(total_deposited)
        .and_then(|x| x.checked_add(total_successful_released))
        .expect("user balance model overflow/underflow");

    assert_eq!(
        get_token_balance(context, &instance_ata),
        expected_instance_balance,
        "final instance balance does not match deposited - released"
    );
    assert_eq!(
        get_token_balance(context, user_ata),
        expected_user_balance,
        "final user balance does not match initial - deposited + released"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Replay a crash file produced by honggfuzz.
    ///
    /// ```sh
    /// CRASH_FILE=hfuzz_workspace/fuzz_escrow/<file>.fuzz \
    ///   RUST_BACKTRACE=1 cargo test --bin fuzz_escrow replay -- --nocapture
    /// ```
    #[test]
    fn replay() {
        let path = std::env::var("CRASH_FILE").expect("set CRASH_FILE env var");
        let data = std::fs::read(&path).expect("could not read crash file");
        let mut u = arbitrary::Unstructured::new(&data);
        if let Ok(input) = FuzzInput::arbitrary(&mut u) {
            let mut ctx = setup_fuzz_context();
            run_fuzz(input, &mut ctx);
        }
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    loop {
        fuzz!(|data: &[u8]| {
            let mut u = arbitrary::Unstructured::new(data);
            if let Ok(input) = FuzzInput::arbitrary(&mut u) {
                let mut ctx = setup_fuzz_context();
                run_fuzz(input, &mut ctx);
            }
        });
    }
}
