//! `compare_balances` pure-function coverage.
//!
//! Two sub-cases are already covered end-to-end by the existing reconciliation
//! suite (`test_reconciliation_passes_within_threshold`,
//! `test_reconciliation_blocks_on_phantom_deposit`).
//! This file adds narrow *unit-of-behaviour* tests for the remaining
//! uncovered branches of `compare_balances`:
//!
//!   A. **Critical mismatch** when `on_chain=0` but `db>0`
//!      (the `u64::MAX` short-circuit).
//!   B. **Both-zero match** (implicit match — no alert even at strict tolerance).
//!   C. **Exactly-at-tolerance** boundary (`delta_bps == tolerance_bps`).
//!
//! Target file: `indexer/src/operator/reconciliation.rs` (compare_balances).
//! Binary: `reconciliation_integration` (existing — but added here as its
//! own `[[test]]` file so it shares the binary's compile without needing
//! the validator fixtures those tests use).

use {
    private_channel_indexer::operator::reconciliation::compare_balances,
    solana_sdk::pubkey::Pubkey, std::collections::HashMap,
};

fn mints(pairs: &[(Pubkey, u64)]) -> HashMap<Pubkey, u64> {
    pairs.iter().copied().collect()
}

#[test]
fn test_compare_balances_critical_mismatch_on_chain_zero() {
    let mint = Pubkey::new_unique();
    // DB says 100 tokens, chain says zero → critical mismatch regardless of
    // tolerance (uses u64::MAX internally).
    let on_chain = mints(&[(mint, 0)]);
    let db = mints(&[(mint, 100)]);

    let mismatches = compare_balances(&on_chain, &db, 10_000 /* 100% tolerance */);
    assert_eq!(
        mismatches.len(),
        1,
        "on_chain=0 + db>0 must always surface a mismatch even at 100% tolerance"
    );
}

#[test]
fn test_compare_balances_both_zero_is_match() {
    let mint = Pubkey::new_unique();
    let on_chain = mints(&[(mint, 0)]);
    let db = mints(&[(mint, 0)]);

    // Strict tolerance; both-zero path must still return no mismatch.
    let mismatches = compare_balances(&on_chain, &db, 0);
    assert!(
        mismatches.is_empty(),
        "both-zero balances must match even at zero tolerance"
    );
}

#[test]
fn test_compare_balances_exactly_at_tolerance_is_match() {
    let mint = Pubkey::new_unique();
    let on_chain = mints(&[(mint, 10_000)]);
    // 1% drift = 100 bps exactly.
    let db = mints(&[(mint, 9_900)]);

    // Tolerance equals the drift → still a match (inclusive boundary).
    let mismatches = compare_balances(&on_chain, &db, 100);
    assert!(
        mismatches.is_empty(),
        "delta_bps == tolerance_bps must be treated as a match (inclusive)"
    );

    // One bps tighter → mismatch surfaces.
    let mismatches_tight = compare_balances(&on_chain, &db, 99);
    assert_eq!(
        mismatches_tight.len(),
        1,
        "tightening tolerance by 1 bps must surface the previously-tolerated drift"
    );
}

// Multi-mint reconciliation with mixed balanced/over/under-tolerance states.
// Exercises the `all_mints.extend(db_balances.keys())` iteration, the
// per-mint selection logic, and `mismatches.push` aggregation across multiple
// entries.
#[test]
fn test_compare_balances_three_mints_mixed_results() {
    let balanced = Pubkey::new_unique();
    let above_tolerance = Pubkey::new_unique();
    let within_tolerance = Pubkey::new_unique();

    // Chain state: all three mints have deposits.
    let on_chain = mints(&[
        (balanced, 10_000),
        (above_tolerance, 10_000),
        (within_tolerance, 10_000),
    ]);

    // DB state:
    //   balanced       → exact match        → no mismatch
    //   above_tol      → 5 % drift (500bps) → over 50-bps tolerance → mismatch
    //   within_tol     → 0.3 % drift (30bps)→ under 50-bps tolerance→ no mismatch
    let db = mints(&[
        (balanced, 10_000),
        (above_tolerance, 9_500),
        (within_tolerance, 9_970),
    ]);

    let mismatches = compare_balances(&on_chain, &db, 50);
    assert_eq!(
        mismatches.len(),
        1,
        "exactly one mint should mismatch at 50-bps tolerance; got {mismatches:?}"
    );
    let mismatch_mint = mismatches[0].mint;
    assert_eq!(
        mismatch_mint, above_tolerance,
        "the reported mismatch must be the above-tolerance mint"
    );
}

// Asymmetric presence. A mint that is in `db_balances`
// but NOT in `on_chain_balances` (and vice versa) must still be considered
// via the `all_mints.extend(db_balances.keys())` branch that seeds the
// iteration set from both maps. This test also documents an important
// nuance: `on_chain=10_000, db=0` produces exactly 10_000 bps drift, which
// is the inclusive upper bound for a 100 % tolerance — so we use a
// slightly tighter tolerance (9_999 bps) to force the mismatch.
#[test]
fn test_compare_balances_mint_only_in_db_is_critical_mismatch() {
    let chain_only = Pubkey::new_unique();
    let db_only = Pubkey::new_unique();

    let on_chain = mints(&[(chain_only, 10_000)]);
    let db = mints(&[(db_only, 100)]);

    // chain_only → DB=0, on_chain=10_000 → 10_000 bps drift; needs tolerance
    //              strictly less than 10_000 bps to be flagged.
    // db_only    → DB=100, on_chain=0   → critical mismatch (u64::MAX delta)
    //              — flagged regardless of tolerance.
    let mismatches = compare_balances(&on_chain, &db, 9_999);
    assert_eq!(
        mismatches.len(),
        2,
        "both asymmetric mints must surface at 9_999-bps tolerance; got {mismatches:?}"
    );
    let seen: std::collections::HashSet<Pubkey> = mismatches.iter().map(|m| m.mint).collect();
    assert!(
        seen.contains(&chain_only),
        "chain-only mint must be in mismatches"
    );
    assert!(
        seen.contains(&db_only),
        "db-only mint (critical) must be in mismatches"
    );
}
