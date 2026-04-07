//! # Fuzz harness for the SMT reset lifecycle
//!
//! ## What this tests
//!
//! Focuses on `ResetSmtRoot` and the cross-tree nonce validity rules:
//!
//! - Nonces from a previous tree generation must always be rejected.
//! - After a reset, fresh nonces in the new generation with valid proofs must succeed.
//! - Token balances are conserved across any number of resets.
//!
//! ## Operations
//!
//! | `Op`                | What it does                                                              |
//! |---------------------|---------------------------------------------------------------------------|
//! | `Deposit`           | User deposits tokens into the escrow ATA.                                 |
//! | `Release`           | Valid release within the current tree generation.                         |
//! | `ResetSmtRoot`      | Operator resets the SMT root and advances the tree index.                 |
//! | `ReleaseStaleNonce` | Attempts a release with a nonce from the previous tree — must always fail.|

use arbitrary::Arbitrary;
use contra_escrow_fuzz::{build_release_ix, clamp_amount, setup_fuzz_context, FuzzContext};
use contra_escrow_program_client::instructions::ResetSmtRootBuilder;
use honggfuzz::fuzz;
use solana_sdk::signature::Signer;
use spl_associated_token_account::get_associated_token_address_with_program_id;
use spl_token::ID as TOKEN_PROGRAM_ID;
use tests_contra_escrow_program::{
    pda_utils::find_event_authority_pda,
    smt_utils::{ProcessorSMT, MAX_TREE_LEAVES},
    state_utils::assert_get_or_deposit,
    utils::{get_token_balance, CONTRA_ESCROW_PROGRAM_ID},
};

// ── Fuzz input ───────────────────────────────────────────────────────────────

#[derive(Arbitrary, Debug, Clone)]
enum Op {
    /// User deposits tokens into the escrow ATA.
    Deposit { amount: u64 },
    /// Valid release within the current tree. Skipped silently when
    /// preconditions aren't met so the harness stays in a clean state.
    Release { amount: u64, nonce_offset: u16 },
    /// Advances the tree index and resets the SMT root.
    ResetSmtRoot,
    /// Attempts a release with a nonce from the previous tree — must always fail.
    ReleaseStaleNonce { nonce_offset: u16 },
}

#[derive(Arbitrary, Debug)]
struct FuzzInput {
    ops: Vec<Op>,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Keep nonce offsets small to avoid building large SMT trees.
fn clamp_nonce_offset(raw: u16) -> u64 {
    (raw as u64) % 100
}

// ── Instruction builder ───────────────────────────────────────────────────────

fn build_reset_ix(ctx: &FuzzContext) -> solana_sdk::instruction::Instruction {
    let (event_authority_pda, _) = find_event_authority_pda();
    ResetSmtRootBuilder::new()
        .payer(ctx.test_context.payer.pubkey())
        .operator(ctx.operator.pubkey())
        .instance(ctx.instance_pda)
        .operator_pda(ctx.operator_pda)
        .event_authority(event_authority_pda)
        .contra_escrow_program(CONTRA_ESCROW_PROGRAM_ID)
        .instruction()
}

// ── Core fuzz logic ───────────────────────────────────────────────────────────

fn run_fuzz(input: FuzzInput, ctx: &mut FuzzContext) {
    let instance_ata = get_associated_token_address_with_program_id(
        &ctx.instance_pda,
        &ctx.mint,
        &TOKEN_PROGRAM_ID,
    );

    let mut current_tree_index: u64 = 0;
    let mut smt = ProcessorSMT::new();
    let mut total_deposited: u64 = 0;
    let mut total_released: u64 = 0;

    for (slot, op) in input.ops.into_iter().take(32).enumerate() {
        // Advance the slot before every operation so repeated transactions with
        // identical parameters get a fresh blockhash and unique signature.
        ctx.test_context.warp_to_slot(slot as u64 + 2);

        match op {
            // ── Deposit ───────────────────────────────────────────────────────
            Op::Deposit { amount } => {
                let deposit_amount = clamp_amount(amount);

                assert_get_or_deposit(
                    &mut ctx.test_context,
                    &ctx.user,
                    &ctx.instance_pda,
                    &ctx.mint,
                    &TOKEN_PROGRAM_ID,
                    deposit_amount,
                    None,
                    false,
                )
                .expect("deposit failed");

                total_deposited = total_deposited
                    .checked_add(deposit_amount)
                    .expect("total_deposited overflow");
            }

            // ── Release ───────────────────────────────────────────────────────
            Op::Release {
                amount,
                nonce_offset,
            } => {
                let release_amount = clamp_amount(amount);
                // Nonces are partitioned by tree generation: generation N owns
                // the range [N * MAX_TREE_LEAVES, (N+1) * MAX_TREE_LEAVES).
                // Using an offset within that range keeps us in the current
                // generation and away from the boundary.
                let nonce =
                    current_tree_index * MAX_TREE_LEAVES as u64 + clamp_nonce_offset(nonce_offset);

                let instance_balance = get_token_balance(&mut ctx.test_context, &instance_ata);

                // Skip silently when preconditions aren't met. Unlike
                // fuzz_escrow, this harness is not testing invalid-proof
                // rejection — that is covered there. Here we only want valid
                // releases so the balance invariant stays easy to track.
                if smt.contains(nonce) || release_amount > instance_balance {
                    continue;
                }

                let (_, proofs) = smt.generate_exclusion_proof(nonce);
                let mut next_smt = smt.clone();
                next_smt.insert(nonce);
                let new_root = next_smt.current_root();

                let release_ix = build_release_ix(
                    &ctx.test_context,
                    &ctx.operator,
                    &ctx.user,
                    ctx.mint,
                    ctx.instance_pda,
                    ctx.operator_pda,
                    ctx.user_ata,
                    instance_ata,
                    release_amount,
                    nonce,
                    new_root,
                    proofs,
                );

                ctx.test_context
                    .send_transaction_with_signers_with_transaction_result(
                        release_ix,
                        &[&ctx.operator],
                        false,
                        Some(1_200_000),
                    )
                    .unwrap_or_else(|e| {
                        panic!(
                            "valid release must succeed: tree={} nonce={} amount={} err={e:?}",
                            current_tree_index, nonce, release_amount
                        )
                    });

                smt.insert(nonce);
                total_released = total_released
                    .checked_add(release_amount)
                    .expect("total_released overflow");
            }

            // ── Reset SMT root ────────────────────────────────────────────────
            //
            // Advances the on-chain tree index and clears the SMT root.
            // `current_tree_index` mirrors the on-chain value so subsequent
            // nonce calculations stay in the correct generation.
            Op::ResetSmtRoot => {
                let reset_ix = build_reset_ix(ctx);

                ctx.test_context
                    .send_transaction_with_signers(reset_ix, &[&ctx.operator])
                    .expect("ResetSmtRoot must always succeed");

                current_tree_index += 1;
                smt = ProcessorSMT::new();
            }

            // ── Stale nonce release ───────────────────────────────────────────
            //
            // Attempts a release using a nonce from the previous generation.
            // The program checks `nonce / MAX_TREE_LEAVES == current_tree_index`
            // before touching the proof, so the proof and amount are irrelevant —
            // the transaction must be rejected solely on the nonce range check.
            Op::ReleaseStaleNonce { nonce_offset } => {
                if current_tree_index == 0 {
                    continue;
                }

                let stale_nonce = (current_tree_index - 1) * MAX_TREE_LEAVES as u64
                    + clamp_nonce_offset(nonce_offset);

                let instance_balance_before =
                    get_token_balance(&mut ctx.test_context, &instance_ata);
                let user_balance_before = get_token_balance(&mut ctx.test_context, &ctx.user_ata);

                let release_ix = build_release_ix(
                    &ctx.test_context,
                    &ctx.operator,
                    &ctx.user,
                    ctx.mint,
                    ctx.instance_pda,
                    ctx.operator_pda,
                    ctx.user_ata,
                    instance_ata,
                    1,
                    stale_nonce,
                    [0xffu8; 32],
                    [0xddu8; 512],
                );

                let result = ctx
                    .test_context
                    .send_transaction_with_signers(release_ix, &[&ctx.operator]);

                assert!(
                    result.is_err(),
                    "stale nonce must be rejected: stale_tree={} nonce={} current_tree={}",
                    current_tree_index - 1,
                    stale_nonce,
                    current_tree_index
                );

                assert_eq!(
                    get_token_balance(&mut ctx.test_context, &instance_ata),
                    instance_balance_before,
                    "instance ATA changed after stale nonce rejection"
                );
                assert_eq!(
                    get_token_balance(&mut ctx.test_context, &ctx.user_ata),
                    user_balance_before,
                    "user ATA changed after stale nonce rejection"
                );
            }
        }
    }

    // ── Final balance invariant ───────────────────────────────────────────────
    let expected = total_deposited
        .checked_sub(total_released)
        .expect("released more than deposited");

    assert_eq!(
        get_token_balance(&mut ctx.test_context, &instance_ata),
        expected,
        "balance mismatch after {} resets: deposited={} released={}",
        current_tree_index,
        total_deposited,
        total_released
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Replay a crash file produced by honggfuzz.
    ///
    /// ```sh
    /// CRASH_FILE=hfuzz_workspace/fuzz_reset_smt/<file>.fuzz \
    ///   RUST_BACKTRACE=1 cargo test --bin fuzz_reset_smt replay -- --nocapture
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
