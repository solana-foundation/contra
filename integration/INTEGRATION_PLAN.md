---
title: Integration Test Coverage Plan (v2)
created: 2026-03-16
updated: 2026-03-18
branch: test/e2e-coverage-improvements
baseline_coverage: 79.0%
target_coverage: 85.0%
baseline_lines_hit: 9706
baseline_lines_total: 12280
coverage_tool: cargo-llvm-cov
coverage_output: coverage/coverage-integration-e2e.lcov
---

# Integration Test Coverage Plan (v2)

## Status Summary

The original plan (v1) targeted 66.4% → 80%. The Makefile wiring and new
test files from that plan are now complete. Re-measuring with all new tests
participating in the coverage sweep is expected to push the LCOV number above
80%. This document sets a new target of **85%** and focuses exclusively on
the remaining gaps.

### What Is Already Done

| Step | Artifact | Tests Added |
|------|----------|-------------|
| Makefile wiring | `integration/Makefile`, root `Makefile` | All 5 previously-registered-but-unrun tests now included in coverage sweep |
| `operator_lifecycle.rs` | 8 tests | deposit, idempotency (2×), alerts, batch, idle, reconciliation webhook, sequential withdrawals |
| `reconciliation_integration` | 4 tests | empty DB, phantom deposit blocked, threshold pass, matching on-chain |
| `gap_detection_integration` | 1 test | restart-gap recovery (Yellowstone path) |
| `mint_idempotency_integration` | 1 test | memo-based idempotency lookup |
| `truncate_integration` | 2 tests | apply-mode, dry-run |

---

## Coverage Baseline (indexer.lcov, pre-new-test run)

Overall: **79.0%** (9 706 / 12 280 lines)

### Files Below 80% — Ordered by Impact

| File | Coverage | Uncovered Lines | Priority |
|------|----------|-----------------|----------|
| `storage/postgres/db.rs` | **0%** | 737 | P1 — fixed by Makefile (no new test) |
| `indexer/datasource/rpc_polling/source.rs` | **0%** | 100 | P1 — needs test |
| `indexer/resync.rs` | **29%** | 96 | P1 — needs test |
| `operator/feepayer_monitor.rs` | **12%** | 44 | P2 — needs test |
| `indexer/datasource/yellowstone/source.rs` | **33%** | 357 | P3 — streaming, hard to cover |
| `operator/utils/signer_util.rs` | **62%** | 112 | P2 — env-var key paths |
| `operator/sender/state.rs` | **65%** | 64 | P2 — SMT init edge cases |
| `operator/sender/transaction.rs` | **72%** | 231 | P2 — retry/error paths |
| `operator/reconciliation.rs` | **79%** | 152 | P3 — close to target |

### Core Files Below 80% (from core-unit.lcov)

| File | Coverage | Uncovered Lines | Notes |
|------|----------|-----------------|-------|
| `core/src/nodes/node.rs` | **32%** | 134 | Read/Write node mode branches |
| `core/src/accounts/get_blocks.rs` | **44%** | 41 | Filter + pagination paths |
| `core/src/accounts/get_blocks_in_range.rs` | **49%** | 33 | Range validation edge cases |
| `core/src/accounts/get_accounts.rs` | **52%** | 28 | Multi-account fetch paths |
| `core/src/accounts/write_batch.rs` | **65%** | 64 | Batch-write error paths |

---

## Invariant Coverage Matrix (current)

All invariants from `docs/INVARIANTS.md` are mapped to their integration test
coverage. MUST-level invariants with no integration test are the top priority.

| ID | Invariant | Level | Integration Test | Gap |
|----|-----------|-------|-----------------|-----|
| C1 | Atomic slot writes | MUST | `contra_integration` (constraint, kill, atomicity) | Adequate |
| C2 | In-memory DB ≥ disk | MUST | `contra_integration` implicit | **No explicit test** |
| C3 | No duplicate signature | MUST | `test_tx_replay`, `test_dedup_persistence` | Strengthen: concurrent |
| C4 | Reject expired blockhash | MUST | `test_blockhash_validation` | Strengthen: exact boundary |
| C5 | All txns must be signed | MUST | None (SDK-level only) | **New test needed** |
| C6 | Instructions allowlist | MUST | None | **New test needed** |
| C7 | Admin sig for admin ix | MUST | `test_non_admin` (1 test) | Strengthen: multi-instruction |
| C8 | Reject mixed admin+user | MUST | `test_mixed_transaction` | Adequate |
| C9 | Reject empty txns | MUST | `test_empty_transaction` | Adequate |
| C10 | Finalized state from DB | MUST | Implicit via C1/C3 | Adequate |
| C11 | Truncation support | SHOULD | `truncate_integration` (2 tests) | Adequate |
| C12 | Backup before truncation | MUST | `test_truncate_apply_mode_e2e` | Adequate |
| C13 | DB backup/recovery | SHOULD | Implicit via truncation | Adequate |
| P1 | Escrow requires SPL | MUST | Implicit in chaos test | **No dedicated test** |
| P2 | Reject unauthorized mints | MUST | Implicit in chaos test | **No dedicated test** |
| P3 | Withdrawal requires admin + proof | MUST | Chaos test + operator_lifecycle | Adequate |
| I1 | Contra indexer < 10s lag | SHOULD | None | **New test (flaky risk)** |
| I2 | Mainnet indexer < 10s lag | SHOULD | None | **New test (flaky risk)** |
| I3 | Backfill missed slots | MUST | `gap_detection_integration` (Yellowstone) | **RPC polling path missing** |
| O1 | No double withdrawal | MUST | `test_withdrawal_operator_prevents_double_withdrawal` | Adequate |
| O2 | No double issuance | MUST | `test_issuance_operator_idempotent_no_double_mint` | Adequate |
| O3 | Alert on failure | MUST | `test_failed_withdrawals_and_mints_fire_alerts` | Adequate |
| G1 | Escrow = liabilities | MUST | `reconciliation_integration` (4 tests) | Strengthen: post-corruption |

---

## Remaining Work — Prioritized

### Tier 1: Highest Impact, Critical

These address 0%-coverage MUST-level gaps and have the highest lines-gained-
per-effort ratio.

---

#### T1-A: `resync_integration` — ResyncService (I3 adjacent, 96 uncovered lines)

**File**: `tests/indexer/resync.rs`
**Cargo.toml**: `[[test]] name = "resync_integration" path = "tests/indexer/resync.rs"`
**Estimated gain**: +85–95 lines (covers `resync.rs` from 29% → ~90%)
**Invariant**: I3 (backfill missed slots — resync is the full-schema-reset path)

ResyncService drops all tables, recreates schema from genesis, then runs a
full backfill. It cannot be unit-tested (requires real DB + RPC + backfill).

| Test | Description | Key Assertions |
|------|-------------|----------------|
| `test_resync_clears_db_and_returns_ok` | Insert 1 dummy row → run resync with `start_slot = current_slot` (no backfill work needed) → verify row gone | `row_count = 0`, `Ok(())` returned |
| `test_resync_rejects_future_genesis_slot` | Call `ResyncService::run(u64::MAX)` on a live validator | Returns `Err(...)` with invalid-genesis-slot message |

```rust
// Key imports
use contra_indexer::{
    config::{BackfillConfig, ProgramType},
    indexer::{
        datasource::rpc_polling::rpc::RpcPoller,
        resync::ResyncService,
    },
    storage::{PostgresDb, Storage},
    PostgresConfig,
};
use test_utils::validator_helper::start_test_validator;
```

---

#### T1-B: `gap_detection_rpc_polling` — RPC polling source (I3, 100 uncovered lines)

**File**: Add test to existing `tests/indexer/gap_detection.rs`
**Cargo.toml**: No change (same test binary)
**Estimated gain**: +70–90 lines (covers `rpc_polling/source.rs` from 0% → ~80%)
**Invariant**: I3 (backfill missed slots — tests the non-Yellowstone datasource path)

The existing `test_gap_detection_restart_recovery` uses Yellowstone geyser.
`RpcPollingSource` (the fallback when no geyser is configured) is 0% covered.

| Test | Description | Key Assertions |
|------|-------------|----------------|
| `test_gap_detection_rpc_polling_fallback` | Same stop/gap/restart scenario as Yellowstone variant but using `start_contra_indexer(None, rpc_url, ...)` (RPC polling mode, `geyser_endpoint = None`) | All 4 deposits in DB after restart; checkpoint advances past gap-slot |

```rust
// Use start_contra_indexer with None for the geyser endpoint
use test_utils::indexer_helper::start_contra_indexer;
// start_contra_indexer(None, rpc_url, db_url) triggers RpcPollingSource
```

---

#### T1-C: `escrow_invariants_integration` — P1, P2 (MUST, on-chain programs)

**File**: `tests/indexer/escrow_invariants.rs`
**Cargo.toml**: `[[test]] name = "escrow_invariants_integration" path = "tests/indexer/escrow_invariants.rs"`
**Estimated gain**: coverage-neutral (exercises programs + indexer parser paths)
**Invariants**: P1 (escrow requires SPL), P2 (rejects unauthorized mints)

These MUST invariants are only exercised implicitly through the chaos test.
Dedicated tests give explicit pass/fail signal and run in isolation.

| Test | Invariant | Description | Key Assertions |
|------|-----------|-------------|----------------|
| `test_spl_deposit_indexed_sol_transfer_ignored` | P1 | Setup instance + indexer. (A) SPL deposit via escrow program → verified indexed. (B) Raw `system_instruction::transfer` to escrow address → NOT indexed | SPL deposit: `type=deposit` in DB. SOL transfer: absent from DB |
| `test_unauthorized_mint_deposit_fails` | P2 | Create instance → create SPL mint NOT added to allowlist → attempt deposit | On-chain tx fails (ProgramError). No row in indexer DB |
| `test_allowed_then_blocked_mint` | P2 | Allow mint → deposit (succeeds) → `block_mint` → deposit again | First deposit in DB, `status=pending`. Second tx fails on-chain. DB unchanged after second attempt |

---

### Tier 2: Important, Medium Effort

---

#### T2-A: `feepayer_monitor_integration` — feepayer health (12% → ~85%)

**File**: Add to `tests/indexer/operator_lifecycle.rs`
**Estimated gain**: +35–40 lines
**Invariant**: O3 adjacent (failed operator health must be observable)

`run_feepayer_monitor` polls the operator's SOL balance and emits a Prometheus
metric. It runs in a background task inside `operator::run`. Testing it directly
avoids the full operator stack.

| Test | Description | Key Assertions |
|------|-------------|----------------|
| `test_feepayer_monitor_reads_balance` | Create operator keypair with known SOL balance. Call `run_feepayer_monitor` with a `CancellationToken` that is pre-cancelled after one iteration | `FEEPAYER_BALANCE_LAMPORTS` metric updated; no panic; returns `Ok(())` |
| `test_feepayer_monitor_low_balance_logs_warning` | Fund feepayer with exactly 0.4 SOL (below 0.5 SOL threshold). Run one iteration | Monitor completes; low-balance warning visible in tracing output (check stdout with `-- --nocapture`) |

```rust
use contra_indexer::operator::{feepayer_monitor::run_feepayer_monitor, RetryConfig, RpcClientWithRetry};
use tokio_util::sync::CancellationToken;
// Pre-cancel the token so the monitor exits after one polling cycle
let token = CancellationToken::new();
token.cancel();
run_feepayer_monitor(config, rpc_client, ProgramType::Escrow, token).await?;
```

---

#### T2-B: C5 + C6 invariant tests — unsigned tx and allowlist (MUST)

**File**: `tests/contra/rpc/test_unsigned_transaction.rs` (C5),
          `tests/contra/rpc/test_allowlist.rs` (C6)
**Wiring**: Add modules to `tests/contra/rpc/mod.rs`; add callables to
            `test_suite()` in `tests/contra/integration.rs`
**Estimated gain**: +20–30 lines (`core/rpc/handler.rs`, `send_transaction_impl.rs`)
**Invariants**: C5 (all txns must be signed), C6 (instructions allowlist enforced)

C5 — Unsigned transaction rejection:

| Test | Description | Key Assertions |
|------|-------------|----------------|
| `test_zero_signatures_rejected` | Manually build `VersionedTransaction` with `num_required_signatures = 0` and a valid SPL instruction; serialize as base64; send via raw JSON-RPC | RPC returns error; balances unchanged |
| `test_tampered_signature_rejected` | Build valid transfer tx; flip one byte in the signature bytes; send raw | Transaction rejected or never confirmed; balances unchanged |

C6 — Allowlist enforcement:

| Test | Description | Key Assertions |
|------|-------------|----------------|
| `test_disallowed_program_rejected` | Create tx with instruction targeting `Pubkey::new_unique()` (random program) | RPC error; no transaction lands |
| `test_vote_program_rejected` | Create tx calling `solana_sdk::vote::program::id()` | RPC error; tx does not land |
| `test_mixed_allowed_disallowed_rejected` | Valid SPL transfer + one fake-program instruction in same tx | Entire tx rejected (allowlist is `.all()` — all-or-nothing) |

---

#### T2-C: G1 strengthen — reconciliation catches corrupted DB

**File**: Add to existing `tests/indexer/reconciliation.rs`
**Estimated gain**: +15–25 lines in `reconciliation.rs`
**Invariant**: G1 (escrow = liabilities; MUST detect divergence)

The current 4 reconciliation tests cover startup reconciliation under controlled
conditions. None verify that tampering with the DB after normal operations is
detected.

| Test | Description | Key Assertions |
|------|-------------|----------------|
| `test_reconciliation_catches_corrupted_db` | Normal flow: start indexer, 3 deposits, operator completes them. Then `UPDATE transactions SET amount = amount * 2` on one row. Run `run_startup_reconciliation(threshold = 0)` | Returns `Err(MismatchExceedsThreshold { count: 1, .. })` |

---

#### T2-D: C3 strengthen — concurrent duplicate submissions

**File**: Add to existing `tests/contra/rpc/test_tx_replay.rs`
**Estimated gain**: +5–10 lines in `stages/dedup.rs`
**Invariant**: C3 (no duplicate signature execution)

The existing replay test sends the same tx sequentially. The concurrent path
(20 goroutines racing to submit the same tx) exercises a different code path
in the dedup stage.

| Test | Description | Key Assertions |
|------|-------------|----------------|
| `test_concurrent_duplicate_submissions` | Build 1 transfer tx → send via 20 concurrent `tokio::spawn` tasks simultaneously | Recipient balance = initial + 1× transfer (not 20×); DB transaction count delta = 1 |

---

### Tier 3: SHOULD-Level, Timing-Sensitive

These address SHOULD-level invariants. Mark `#[ignore]` in CI; run manually
or in a nightly job with relaxed thresholds.

---

#### T3-A: `indexer_lag_integration` — I1, I2 (SHOULD, timing-sensitive)

**File**: `tests/indexer/indexer_lag.rs`
**Cargo.toml**: `[[test]] name = "indexer_lag_integration" path = "tests/indexer/indexer_lag.rs"`
**Invariants**: I1 (Contra indexer < 10s behind), I2 (Mainnet indexer < 10s behind)

Mark both tests `#[ignore]` to keep CI deterministic; run with
`cargo test --test indexer_lag_integration -- --ignored --nocapture`.

| Test | Invariant | Description | Key Assertions |
|------|-----------|-------------|----------------|
| `test_contra_indexer_tracks_head` | I1 | Full stack (Contra node + indexer). Send 20 txns over 5s. Read node's current slot + indexer checkpoint from `indexer_state` | Slot gap < 100 (with blocktime_ms=100, 100 slots = 10s) |
| `test_l1_indexer_tracks_head` | I2 | Test validator + L1 indexer (Yellowstone). Execute 5 deposits → wait 3s → compare validator slot and L1 checkpoint | `validator_slot - indexer_checkpoint_slot < 100` |

---

#### T3-B: C2 — in-memory DB sync

**File**: Add callables to `tests/contra/rpc/test_spl_token.rs`, called from `test_suite()`
**Invariant**: C2 (in-memory DB must be ≥ disk state)

| Test | Description | Key Assertions |
|------|-------------|----------------|
| `test_read_after_write_consistency` | Mint tokens → immediately read balance via RPC (no sleep) | `get_token_balance == expected` before any settlement flush |
| `test_state_survives_restart` | Create mint, transfer, restart node against same DB, read balance | Balances match pre-restart values |

---

## Cargo.toml Additions Required

```toml
[[test]]
name = "resync_integration"
path = "tests/indexer/resync.rs"

[[test]]
name = "escrow_invariants_integration"
path = "tests/indexer/escrow_invariants.rs"

[[test]]
name = "indexer_lag_integration"
path = "tests/indexer/indexer_lag.rs"
```

---

## Makefile Additions Required

### `integration/Makefile` — `integration-test` target

```makefile
@cargo test --test resync_integration -- --nocapture
@cargo test --test escrow_invariants_integration -- --nocapture
# indexer_lag runs ignored by default; add --ignored for nightly:
# @cargo test --test indexer_lag_integration -- --ignored --nocapture
```

### `integration/Makefile` — `integration-coverage-indexer` target

```makefile
@cargo llvm-cov test --no-report --workspace --test resync_integration -- --nocapture
@cargo llvm-cov test --no-report --workspace --test escrow_invariants_integration -- --nocapture
```

---

## Implementation Order

| Priority | ID | Task | Invariants | Uncovered Lines Gained | Effort |
|----------|----|------|------------|------------------------|--------|
| 1 | T1-A | `resync_integration` (2 tests) | I3 | +85–95 | 2–3 h |
| 2 | T1-B | RPC polling gap test (extend `gap_detection.rs`) | I3 | +70–90 | 1–2 h |
| 3 | T1-C | `escrow_invariants_integration` (3 tests) | P1, P2 | program coverage | 3–4 h |
| 4 | T2-A | Feepayer monitor tests (extend `operator_lifecycle.rs`) | O3 adj | +35–40 | 1–2 h |
| 5 | T2-B | C5 + C6 invariant tests (2 new files) | C5, C6 | +20–30 | 2–3 h |
| 6 | T2-C | G1 strengthen (extend `reconciliation.rs`) | G1 | +15–25 | 1 h |
| 7 | T2-D | C3 concurrent dedup (extend `test_tx_replay.rs`) | C3 | +5–10 | 1 h |
| 8 | T3-A | `indexer_lag_integration` (2 tests, `#[ignore]`) | I1, I2 | timing only | 2 h |
| 9 | T3-B | C2 memory-sync tests (extend `test_spl_token.rs`) | C2 | +10–20 | 1–2 h |

---

## Expected Coverage After All Tiers

| Tier Complete | Estimated Coverage |
|---------------|--------------------|
| Baseline (current lcov) | 79.0% |
| After Makefile wiring lands (new tests counted) | ~81–82% |
| After Tier 1 (T1-A, T1-B, T1-C) | ~83–84% |
| After Tier 2 (T2-A through T2-D) | ~85–86% |
| After Tier 3 | ~86% (SHOULD-level, marginal gain) |

---

## Verification Commands

```bash
# Build check for all test binaries (fast)
cd /root/contra/integration
cargo build --tests 2>&1 | tail -5

# Run each new test individually
cargo test --test resync_integration -- --nocapture
cargo test --test escrow_invariants_integration -- --nocapture
cargo test --features test-tree --test operator_lifecycle_integration -- --nocapture

# Run lag tests (ignored in CI, manual only)
cargo test --test indexer_lag_integration -- --ignored --nocapture

# Full E2E coverage sweep
make integration-coverage

# Parse result
awk -F: '/^LF:/{t+=$2} /^LH:/{h+=$2} END{printf "Coverage: %.1f%% (%d/%d lines)\n",h*100/t,h,t}' \
  /root/contra/coverage/coverage-integration-e2e.lcov
```

---

## Coverage Summary: What Is and Is Not Covered

### Indexer — Covered by Integration Tests (>80%)

| File | Coverage | Covered By |
|------|----------|-----------|
| `storage/postgres/db.rs` | 0% → **~70%*** | reconciliation, gap_detection, mint_idempotency (now in sweep) |
| `operator/processor.rs` | 89% | operator_lifecycle (deposit + withdrawal routing) |
| `operator/sender/mint.rs` | 87% | operator_lifecycle (idempotency, batch) |
| `indexer/backfill.rs` | 93% | gap_detection, indexer_integration chaos |
| `operator/fetcher.rs` | 94% | operator_lifecycle, indexer_integration |
| `operator/reconciliation.rs` | 79% | operator_lifecycle (reconciliation webhook test) |

*\* Not yet re-measured; will improve once new tests run in coverage sweep*

### Indexer — NOT Covered / Needs New Tests

| File | Coverage | Missing Test |
|------|----------|-------------|
| `storage/postgres/db.rs` | 0% (737 lines) | Requires new tests in coverage sweep — T1 fix |
| `indexer/datasource/rpc_polling/source.rs` | **0%** (100 lines) | T1-B: RPC polling gap test |
| `indexer/resync.rs` | **29%** (96 uncovered) | T1-A: `resync_integration` |
| `operator/feepayer_monitor.rs` | **12%** (44 uncovered) | T2-A: feepayer monitor tests |
| `indexer/datasource/yellowstone/source.rs` | 33% (357 uncovered) | Hard — streaming geyser, not easily testable |
| `operator/utils/signer_util.rs` | 62% (112 uncovered) | env-var key loading paths, OS-level dependency |

### Contra Core — Covered by Integration Tests (>80%)

| File | Coverage | Covered By |
|------|----------|-----------|
| `stages/dedup.rs` | 86% | contra_integration (replay, dedup persistence) |
| `stages/settle.rs` | 86% | contra_integration (SPL token, full round-trip) |
| `stages/execution.rs` | 82% | contra_integration (multiple transaction types) |
| `rpc/handler.rs` | 80% | contra_integration (RPC method coverage) |
| `accounts/bob.rs` | 88% | contra_integration (in-memory reads/writes) |
| `accounts/truncate.rs` | 88% | truncate_integration (apply + dry-run) |

### Contra Core — NOT Covered / Needs New Tests

| File | Coverage | Missing Test |
|------|----------|-------------|
| `nodes/node.rs` | **32%** (134 uncovered) | Read-only and Write-only node mode branches not tested |
| `accounts/get_blocks.rs` | **44%** (41 uncovered) | Filter/pagination edge cases |
| `accounts/get_blocks_in_range.rs` | **49%** (33 uncovered) | Range boundary conditions |
| `accounts/write_batch.rs` | **65%** (64 uncovered) | Multi-backend write error paths |
| `accounts/store_block.rs` | **69%** (20 uncovered) | Block overwrite and error paths |

### On-Chain Programs — NOT Covered by Integration Tests

| Invariant | Status | Missing Test |
|-----------|--------|-------------|
| P1: Escrow requires SPL | Implicit only | T1-C: `test_spl_deposit_indexed_sol_transfer_ignored` |
| P2: Reject unauthorized mints | Implicit only | T1-C: `test_unauthorized_mint_deposit_fails`, `test_allowed_then_blocked_mint` |

### Contra Core Invariants — NOT Covered by Explicit Integration Tests

| Invariant | Level | Status | Missing Test |
|-----------|-------|--------|-------------|
| C5: All txns must be signed | MUST | No explicit test | T2-B: `test_zero_signatures_rejected`, `test_tampered_signature_rejected` |
| C6: Instructions allowlist | MUST | No explicit test | T2-B: `test_disallowed_program_rejected`, `test_vote_program_rejected`, `test_mixed_allowed_disallowed_rejected` |
| C2: In-memory ≥ disk | MUST | No explicit test | T3-B: `test_read_after_write_consistency` |
| I1: Contra indexer lag < 10s | SHOULD | No test | T3-A: `test_contra_indexer_tracks_head` |
| I2: Mainnet indexer lag < 10s | SHOULD | No test | T3-A: `test_l1_indexer_tracks_head` |
