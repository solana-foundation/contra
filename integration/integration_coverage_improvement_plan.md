---
title: Integration Coverage Improvement Plan
created: 2026-03-19
branch: test/e2e-coverage-improvements
measured_coverage: 65.76% (7,449 / 11,328 lines)
target_coverage: 80%+
coverage_tool: cargo-llvm-cov
coverage_report: coverage/coverage-integration-e2e.lcov
---

# Integration Coverage Improvement Plan

## 1. Coverage Baseline & Discrepancy Explanation

The LCOV report at `coverage/coverage-integration-e2e.lcov` measures **65.76%**
(7,449 / 11,328 lines). `INTEGRATION_PLAN.md` records 79.0% as its baseline — a
~13 pp discrepancy. The difference comes from two sources:

1. **Stale LCOV**: The `.lcov` file was generated before the current branch's
   Makefile changes. The new `--ignore-filename-regex` in `integration/Makefile`
   (line 71) already excludes binary entry points and infrastructure stubs.
   Re-measuring with the current Makefile will eliminate those 0%-coverage files
   from the denominator and raise the number meaningfully before any new tests
   are written.

2. **Different scope**: The 79.0% figure in `INTEGRATION_PLAN.md` was measured
   from `coverage/coverage-indexer.lcov` (indexer crate only). The E2E LCOV
   includes `contra-core` as well, which brings in lower-coverage RPC handler
   and account storage files, pulling the composite percentage down.

**First action before writing any new test**: re-run `make integration-coverage`
to get a fresh, accurate baseline with the current ignore patterns. Estimated
true baseline is **~72–74%**.

---

## 2. Known Makefile Bug: `reconciliation_e2e_test` Not Wired in Cargo.toml

`integration/Makefile` line 64 runs:

```makefile
@cargo llvm-cov test --no-report --workspace --test reconciliation_e2e_test -- --nocapture
```

There is no `[[test]] name = "reconciliation_e2e_test"` entry in
`integration/Cargo.toml` and no corresponding source file exists. This command
will silently succeed (cargo finds no matching test binary and exits 0) but
accumulates zero coverage data. Fix this before measuring the true baseline:

**Option A** — Remove the dead line from the Makefile if the test was merged
into `reconciliation_integration`.

**Option B** — Create the file, wire it in `Cargo.toml`, and implement the test
(see T2-C below — reconciliation DB-corruption detection — which is the natural
content for this binary).

---

## 3. Files Currently in LCOV That Should Be Excluded

The current `--ignore-filename-regex` in `integration/Makefile:71` already
excludes these, but the checked-in LCOV pre-dates those changes. Verify they are
absent after re-measuring. If any still appear, extend the regex.

| File | Lines | Reason |
|------|-------|--------|
| `indexer/src/bin/indexer.rs` | 295 | Binary entry point (`fn main`) |
| `indexer/src/bin/generate_transactions/main.rs` | 174 | Dev-only utility binary |
| `indexer/src/bin/generate_transactions/helpers.rs` | 89 | Dev-only utility helper |
| `indexer/src/shutdown_utils.rs` | 334 | OS-signal plumbing, not unit-testable |
| `indexer/src/metrics.rs` | 60 | Prometheus metric registration stubs |
| `core/src/test_helpers.rs` | 23 | Test infrastructure — not production code |
| `core/src/scheduler/greedy.rs` | 62 | Unused alternate scheduler |
| `core/src/client.rs` | 2 | Thin wrapper with no testable logic |

**Estimated coverage after exclusion only (no new tests): ~72–74%**

Current ignore regex (already in Makefile):
```
'(^tests/|/test_utils/|\.cargo/registry|src/bin/|shutdown_utils\.rs|metrics\.rs|test_helpers\.rs|scheduler/greedy\.rs|core/src/client\.rs)'
```

---

## 4. Additional Candidates for Exclusion

These files have very low coverage and no meaningful path to coverage. Excluding
them is the right call rather than writing fragile tests to chase the numbers.

| File | Coverage | Lines | Justification |
|------|----------|-------|---------------|
| `core/src/accounts/error.rs` | 33% | 9 | Error type definitions — no executable logic |
| `indexer/src/operator/constants.rs` | 0% | 12 | Pure constant declarations |
| `indexer/src/storage/common/storage/close.rs` | 0% | 7 | DB close (called from shutdown path) |
| `indexer/src/storage/common/storage/drop_tables.rs` | 0% | 7 | Only called by resync — covered indirectly by T1-A |
| `indexer/src/indexer/datasource/common/parser/pubkey.rs` | 0% | 8 | Single-line parse utility |

Add to the ignore regex:
```
|accounts/error\.rs|operator/constants\.rs|storage/common/storage/close\.rs|storage/common/storage/drop_tables\.rs|parser/pubkey\.rs
```

**Additional coverage gain from these exclusions: ~+0.3 pp** (small but clean).

---

## 5. Files That Are Hard to Cover (Do Not Pursue)

| File | Coverage | Why Not Worth Pursuing |
|------|----------|------------------------|
| `indexer/datasource/yellowstone/source.rs` | 62% | Internal gRPC streaming reconnect logic; requires live Yellowstone infra |
| `indexer/src/config.rs` | 52% | Config-parsing branches gated on env-var combinations; testing all combos has diminishing returns |
| `indexer/datasource/common/parser/escrow.rs` | 79% | Already near target; remaining 21% is rarely-used instruction variants |

---

## 6. Gap Analysis — Per-File Coverage Below 80%

Files ordered by uncovered lines (impact descending). This is the source of
truth for which new tests to write.

### Indexer Crate

| File | Coverage | Uncovered Lines | New Test Needed |
|------|----------|-----------------|-----------------|
| `indexer/src/operator/sender/mint.rs` | 52.6% | 201 | T-MINT: mint retry, over-issuance guard, batch paths |
| `indexer/src/operator/utils/signer_util.rs` | 23.8% | 122 | T-SIGNER: env-var key loading, fallback paths |
| `indexer/src/operator/sender/transaction.rs` | 65.2% | 138 | T-TX: retry-on-timeout, send-failure, confirmation loop |
| `indexer/src/indexer/datasource/yellowstone/source.rs` | 62.3% | 148 | Skip (hard to test, see §5) |
| `indexer/src/indexer/resync.rs` | 0%\* | 111 | **T1-A**: `resync_integration` |
| `indexer/src/indexer/indexer.rs` | 62.3% | 66 | Covered partially by T1-B (RPC path) |
| `indexer/src/operator/sender/proof.rs` | 46.7% | 64 | T-PROOF: SMT proof construction edge cases |
| `indexer/src/operator/sender/state.rs` | 62.7% | 38 | T-TX covers some; resync-state path via T1-A |
| `indexer/src/operator/sender/mod.rs` | 57.1% | 24 | Covered by T-MINT + T-TX |
| `indexer/src/operator/utils/mint_util.rs` | 58.9% | 37 | Covered by T-MINT |
| `indexer/src/operator/reconciliation.rs` | 70.6% | 77 | T2-C: corruption-detection test |
| `indexer/src/indexer/backfill.rs` | 75.4% | 47 | Partially covered by T1-B |
| `indexer/src/indexer/checkpoint.rs` | 75.0% | 24 | Covered by T1-A + T1-B |
| `indexer/src/operator/feepayer_monitor.rs` | 75.0% | 11 | T2-A: one targeted test |
| `indexer/src/indexer/datasource/rpc_polling/source.rs` | 77.0% | 23 | **T1-B**: RPC polling gap test |

\* The INTEGRATION_PLAN.md records `resync.rs` at 29% (96 uncovered) from a prior
measurement; the E2E LCOV shows 0%. T1-A addresses both.

### Core Crate

| File | Coverage | Uncovered Lines | New Test Needed |
|------|----------|-----------------|-----------------|
| `core/src/rpc/handler.rs` | 32.5% | 83 | **T2-B**: C5/C6 invariant tests |
| `core/src/rpc/simulate_transaction_impl.rs` | 51.7% | 101 | T-SIM: error/edge-case paths |
| `core/src/accounts/get_blocks_in_range.rs` | 49.2% | 33 | T-BLOCKS: range boundary conditions |
| `core/src/rpc/send_transaction_impl.rs` | 56.6% | 36 | **T2-B**: unsigned/disallowed tx paths |
| `core/src/accounts/get_blocks.rs` | 64.7% | 12 | T-BLOCKS: filter/pagination |
| `core/src/nodes/node.rs` | 72.9% | 45 | T-NODE: Read/Write mode branches |
| `core/src/stages/sequencer.rs` | 70.7% | 27 | Extend contra_integration |
| `core/src/stages/settle.rs` | 70.1% | 87 | Extend contra_integration |
| `core/src/webhook.rs` | 62.2% | 31 | Extend operator_lifecycle webhook tests |
| `core/src/scheduler/dag.rs` | 63.4% | 48 | Low-priority, scheduling internals |

---

## 7. Implementation Tasks

Tasks are ordered by lines-gained-per-effort. Tiers 1 and 2 are sufficient to
reach 80%. Tier 3 pushes to 85%+.

### Phase 0: Infrastructure (No New Tests)

**Before implementing any new test**, complete these non-test tasks:

| Task | Action | Owner |
|------|--------|-------|
| P0-1 | Re-run `make integration-coverage` to get a fresh baseline | DevOps / any |
| P0-2 | Fix `reconciliation_e2e_test` Makefile/Cargo.toml mismatch (§2) | Test eng |
| P0-3 | Add small-file exclusions to `--ignore-filename-regex` (§4) | Test eng |

Expected coverage after P0: **~72–74%** (from exclusions + fresh measurement).

---

### Tier 1 — Critical (72–74% → ~77%)

---

#### T1-A: `resync_integration` — ResyncService (I3)

**Status**: Not implemented. Referenced in `INTEGRATION_PLAN.md` but no file exists.
**Cargo.toml**: Add `[[test]] name = "resync_integration" path = "tests/indexer/resync.rs"`
**Makefile**: Add to `integration-coverage-indexer` target
**Estimated gain**: +95–110 covered lines in `indexer/src/indexer/resync.rs`

`ResyncService` drops all DB tables, recreates schema, then runs a full backfill.
It cannot be unit-tested (requires a real DB + RPC + backfill) which is why it
has 0% integration coverage today.

| Test | Scenario | Key Assertions |
|------|----------|----------------|
| `test_resync_clears_db_and_returns_ok` | Insert 1 dummy deposit row → call `ResyncService::run(start_slot = current_slot)` (no real backfill needed) | Row count = 0 after resync; function returns `Ok(())` |
| `test_resync_rejects_invalid_genesis_slot` | Call `ResyncService::run(u64::MAX)` on a live validator | Returns `Err(...)` containing an invalid-genesis-slot message |

**Key imports**:
```rust
use contra_indexer::{
    config::{BackfillConfig, ProgramType},
    indexer::{datasource::rpc_polling::rpc::RpcPoller, resync::ResyncService},
    storage::{PostgresDb, Storage},
    PostgresConfig,
};
use test_utils::validator_helper::start_test_validator;
```

---

#### T1-B: RPC Polling Gap Detection (I3)

**Status**: `gap_detection_integration` exists and runs, but only tests the Yellowstone
path. `rpc_polling/source.rs` is at 77% (23 uncovered lines) and `indexer/indexer.rs`
is at 62% (66 uncovered lines) because they are never exercised via the RPC polling
datasource.
**Cargo.toml**: No change (extends existing `gap_detection_integration` binary)
**Estimated gain**: +80–100 covered lines across `rpc_polling/source.rs` and `indexer/indexer.rs`

| Test | Scenario | Key Assertions |
|------|----------|----------------|
| `test_gap_detection_rpc_polling_fallback` | Same stop/gap/restart scenario as the Yellowstone variant, but calls `start_contra_indexer(None, rpc_url, db_url)` (RPC polling mode, `geyser_endpoint = None`) | All deposits land in DB after restart; checkpoint slot advances past the gap |

**Key difference from existing test**: pass `None` as the geyser endpoint to
`start_contra_indexer` so `RpcPollingSource` is selected instead of Yellowstone.

---

#### T1-C: Sender Retry & Error Paths (operator/sender/transaction.rs, sender/mint.rs)

**Status**: `operator_lifecycle_integration` covers the happy path for both mint
and send. The retry-on-timeout, confirmation-loop, and error-propagation branches
in `sender/transaction.rs` (138 uncovered lines) and `sender/mint.rs` (201 uncovered
lines) are unreached.
**Cargo.toml**: No change (extends `operator_lifecycle_integration`)
**Estimated gain**: +150–180 covered lines

| Test | Scenario | Key Assertions |
|------|----------|----------------|
| `test_send_retries_on_rpc_timeout` | Inject a mock RPC that returns timeout errors for the first 2 attempts, then succeeds | Transaction lands; retry counter > 1; no duplicate sends |
| `test_mint_over_issuance_guard` | Attempt to mint to an already-completed withdrawal (duplicate) | Second mint not submitted; idempotency check triggers; DB row status unchanged |
| `test_mint_batch_multiple_withdrawals` | Queue 5 withdrawals; allow batch sender to process in one cycle | All 5 rows reach `status = completed`; single batch mint tx on-chain |
| `test_send_confirmation_timeout_marks_failed` | Inject an RPC that never confirms; confirmation loop exceeds deadline | Row marked `status = failed`; no panic; monitor alert emitted |

**Note**: Injecting RPC failures requires either a `mockito` HTTP mock (already
a dependency in `Cargo.toml`) or a `CancellationToken` to cap the confirmation
wait. Use `mockito` for network-level failures; use a pre-cancelled token for
deadline tests.

---

### Tier 2 — Important (77% → ~80%)

---

#### T2-A: Feepayer Monitor (O3 adjacent)

**Status**: `feepayer_monitor.rs` is at 75% with 11 uncovered lines. One targeted
test closes the gap.
**Cargo.toml**: No change (extends `operator_lifecycle_integration`)
**Estimated gain**: +10–12 covered lines

| Test | Scenario | Key Assertions |
|------|----------|----------------|
| `test_feepayer_monitor_reads_balance` | Fund operator keypair with known SOL balance; call `run_feepayer_monitor` with a pre-cancelled `CancellationToken` | `FEEPAYER_BALANCE_LAMPORTS` Prometheus metric updated; returns `Ok(())` without panic |

```rust
use contra_indexer::operator::feepayer_monitor::run_feepayer_monitor;
use tokio_util::sync::CancellationToken;
// Pre-cancel so the monitor exits after exactly one polling cycle.
let token = CancellationToken::new();
token.cancel();
run_feepayer_monitor(config, rpc_client, ProgramType::Escrow, token).await?;
```

---

#### T2-B: C5 + C6 Invariant Tests — Unsigned TX & Allowlist (MUST)

**Status**: No explicit integration test covers these two MUST-level invariants.
`core/src/rpc/handler.rs` is at 32.5% (83 uncovered lines); `send_transaction_impl.rs`
is at 56.6% (36 uncovered lines). These are the two highest-impact core files.
**Files to create**:
- `tests/contra/rpc/test_unsigned_transaction.rs` (C5)
- `tests/contra/rpc/test_allowlist.rs` (C6)

Wire both into `tests/contra/rpc/mod.rs` and call from `test_suite()` in
`tests/contra/integration.rs`.
**Estimated gain**: +80–100 covered lines in `handler.rs` and `send_transaction_impl.rs`

**C5 — Unsigned transaction rejection**:

| Test | Scenario | Key Assertions |
|------|----------|----------------|
| `test_zero_signatures_rejected` | Build `VersionedTransaction` with `num_required_signatures = 0` and a valid SPL instruction; send via raw JSON-RPC | RPC returns error code; recipient balance unchanged |
| `test_tampered_signature_rejected` | Build valid transfer tx; flip one byte in the signature; send | Transaction rejected or never confirmed; balances unchanged |

**C6 — Instruction allowlist enforcement**:

| Test | Scenario | Key Assertions |
|------|----------|----------------|
| `test_disallowed_program_rejected` | TX with instruction targeting `Pubkey::new_unique()` (random program) | RPC error; no transaction lands |
| `test_vote_program_rejected` | TX calling `solana_sdk::vote::program::id()` | RPC error |
| `test_mixed_allowed_disallowed_rejected` | Valid SPL transfer + one fake-program instruction in same TX | Entire TX rejected (allowlist is all-or-nothing) |

---

#### T2-C: G1 Reconciliation — DB Corruption Detection

**Status**: `INTEGRATION_PLAN.md` specifies this; the `reconciliation_e2e_test`
Makefile line should wire this binary (resolves the P0-2 bug simultaneously).
**Files**: `tests/indexer/reconciliation_e2e.rs` (new) wired as `reconciliation_e2e_test`
in both `Cargo.toml` and `integration-coverage-indexer` in the Makefile.
**Estimated gain**: +20–25 covered lines in `indexer/src/operator/reconciliation.rs`

| Test | Scenario | Key Assertions |
|------|----------|----------------|
| `test_reconciliation_catches_corrupted_db` | Normal flow: start indexer, 3 deposits, operator completes them. Then `UPDATE transactions SET amount = amount * 2` on one row. Run `run_startup_reconciliation(threshold = 0)` | Returns `Err(MismatchExceedsThreshold { count: 1, .. })` |

---

#### T2-D: `get_blocks_in_range` Edge Cases

**Status**: `core/src/accounts/get_blocks_in_range.rs` is at 49.2% (33 uncovered
lines). The existing `test_get_blocks.rs` exercises the happy path. Range
boundary and error paths are untested.
**Cargo.toml**: No change (extends `contra_integration` via `test_get_blocks.rs`)
**Estimated gain**: +25–30 covered lines

| Test | Scenario | Key Assertions |
|------|----------|----------------|
| `test_get_blocks_end_before_start` | Call `get_blocks(start=100, end=Some(50))` | Returns RPC error with "end_slot must be >= start_slot" message |
| `test_get_blocks_range_too_large` | Call `get_blocks` with `end - start > MAX_SLOT_RANGE` | Returns RPC error with "Slot range too large" message |

These two paths are in `get_blocks_impl.rs` (lines 17–33) which feed into
`get_blocks_in_range.rs`. Add them as sub-cases inside `run_get_blocks_test`.

---

#### T2-E: C3 Concurrent Dedup (strengthen)

**Status**: `test_tx_replay.rs` tests sequential replay. The concurrent submission
path in `stages/dedup.rs` is a different code branch.
**Cargo.toml**: No change (extends `contra_integration`)
**Estimated gain**: +8–12 covered lines in `core/src/stages/sigverify.rs`

| Test | Scenario | Key Assertions |
|------|----------|----------------|
| `test_concurrent_duplicate_submissions` | Build 1 transfer TX → submit via 20 concurrent `tokio::spawn` tasks simultaneously | Recipient balance = initial + 1× transfer (not 20×); DB transaction count delta = 1 |

---

### Tier 3 — Stretch (80% → 85%)

These push beyond 80% and cover SHOULD-level invariants. Mark timing-sensitive
tests `#[ignore]` in CI; run in nightly or manual jobs.

---

#### T3-A: `indexer_lag_integration` — I1, I2 (SHOULD, timing-sensitive)

**File**: `tests/indexer/indexer_lag.rs`
**Cargo.toml**: `[[test]] name = "indexer_lag_integration" path = "tests/indexer/indexer_lag.rs"`
Mark both tests `#[ignore]`.

| Test | Invariant | Scenario | Key Assertions |
|------|-----------|----------|----------------|
| `test_contra_indexer_tracks_head` | I1 | Full stack. Send 20 txns over 5s. Compare node slot vs indexer checkpoint | `slot_gap < 100` (100 slots × 100ms blocktime = 10s) |
| `test_l1_indexer_tracks_head` | I2 | Test validator + L1 indexer (Yellowstone). 5 deposits → wait 3s → compare | `validator_slot - indexer_checkpoint_slot < 100` |

---

#### T3-B: C2 In-Memory DB Sync (MUST, low effort)

**File**: Extend `tests/contra/rpc/test_spl_token.rs`
**Estimated gain**: +15–20 covered lines in `core/src/accounts/bob.rs`

| Test | Invariant | Scenario | Key Assertions |
|------|-----------|----------|----------------|
| `test_read_after_write_consistency` | C2 | Mint tokens → immediately read balance (no sleep) | `get_token_balance == expected` before any settlement flush |
| `test_state_survives_restart` | C2 | Create mint, transfer, restart node against same DB, read balance | Balances match pre-restart values |

---

#### T3-C: `nodes/node.rs` Read/Write Mode Branches

**Status**: `core/src/nodes/node.rs` is at 72.9% (45 uncovered lines). Only
all-in-one (`Aio`) mode is tested. Read-only and Write-only mode startup branches
are untouched.
**Estimated gain**: +35–40 covered lines

| Test | Scenario | Key Assertions |
|------|----------|----------------|
| `test_read_only_node_rejects_writes` | Start node in `Read` mode; attempt `sendTransaction` | Returns "write endpoint not available" or equivalent error |
| `test_write_only_node_rejects_reads` | Start node in `Write` mode; attempt `getSlot` or `getBalance` | Returns "read endpoint not available" or equivalent error |

---

#### T3-D: Escrow Invariants P1 + P2

**Status**: From `INTEGRATION_PLAN.md` T1-C. Coverage-neutral for the LCOV (exercises
on-chain programs), but provides explicit invariant signal rather than implicit chaos-test coverage.
**File**: `tests/indexer/escrow_invariants.rs`
**Cargo.toml**: `[[test]] name = "escrow_invariants_integration" path = "tests/indexer/escrow_invariants.rs"`

| Test | Invariant | Scenario |
|------|-----------|----------|
| `test_spl_deposit_indexed_sol_transfer_ignored` | P1 | SPL deposit via escrow program → indexed. Raw SOL transfer to escrow address → NOT indexed |
| `test_unauthorized_mint_deposit_fails` | P2 | Mint NOT on allowlist → deposit fails on-chain; no DB row |
| `test_allowed_then_blocked_mint` | P2 | Allow mint → deposit succeeds → `block_mint` → second deposit fails; DB row count unchanged |

---

## 8. Cargo.toml Additions Required

```toml
# T1-A
[[test]]
name = "resync_integration"
path = "tests/indexer/resync.rs"

# T2-C (also fixes the reconciliation_e2e_test Makefile bug)
[[test]]
name = "reconciliation_e2e_test"
path = "tests/indexer/reconciliation_e2e.rs"

# T3-A
[[test]]
name = "indexer_lag_integration"
path = "tests/indexer/indexer_lag.rs"

# T3-D
[[test]]
name = "escrow_invariants_integration"
path = "tests/indexer/escrow_invariants.rs"
```

---

## 9. Makefile Additions Required

### `integration-test` target — append:

```makefile
@echo "Running resync integration tests..."
@cargo test --test resync_integration -- --nocapture
@echo "Running reconciliation E2E tests..."
@cargo test --test reconciliation_e2e_test -- --nocapture
@echo "Running escrow invariant tests..."
@cargo test --test escrow_invariants_integration -- --nocapture
# Lag tests run ignored; uncomment for nightly:
# @cargo test --test indexer_lag_integration -- --ignored --nocapture
```

### `integration-coverage-indexer` target — append:

```makefile
@echo "Running resync integration tests with coverage..."
@cargo llvm-cov test --no-report --workspace --test resync_integration -- --nocapture
@echo "Running reconciliation E2E tests with coverage..."
@cargo llvm-cov test --no-report --workspace --test reconciliation_e2e_test -- --nocapture
@echo "Running escrow invariant tests with coverage..."
@cargo llvm-cov test --no-report --workspace --test escrow_invariants_integration -- --nocapture
```

---

## 10. Implementation Order & Coverage Projections

| Step | Tasks | Est. Coverage | Cumulative |
|------|-------|--------------|------------|
| **P0** | Re-measure, fix Makefile bug, add exclusions | ~72–74% | ~73% |
| **T1-A** | `resync_integration` (2 tests) | +1.0–1.2 pp | ~74% |
| **T1-B** | RPC polling gap test (1 test) | +0.9–1.1 pp | ~75% |
| **T1-C** | Sender retry + mint batch (4 tests) | +1.6–2.0 pp | ~77% |
| **T2-B** | C5 + C6 invariants (5 tests) | +0.9–1.1 pp | ~78% |
| **T2-D** | `get_blocks_in_range` edge cases (2 tests) | +0.3–0.4 pp | ~78.4% |
| **T2-C** | Reconciliation corruption (1 test) | +0.2–0.3 pp | ~78.7% |
| **T2-A** | Feepayer monitor (1 test) | +0.1–0.2 pp | ~78.9% |
| **T2-E** | Concurrent dedup (1 test) | +0.1 pp | ~79% |
| **T3-C** | Node Read/Write mode (2 tests) | +0.4–0.5 pp | **~79.5%** |
| **T3-B** | C2 memory-sync (2 tests) | +0.2 pp | **~79.7%** |
| **T3-A** | Lag tests (`#[ignore]`) | timing only | ~79.7% |
| **T3-D** | Escrow invariants (3 tests) | program coverage | ~80%+ |

Completing P0 through T2-E (16 tests) should reliably reach **~79–80%**.
Adding T3-C and T3-B pushes past 80% with a comfortable margin.

---

## 11. Verification Commands

```bash
# Step 1: Measure true baseline after P0 changes
cd /root/contra/integration
make integration-coverage
awk -F: '/^LF:/{t+=$2} /^LH:/{h+=$2} END{printf "Coverage: %.1f%% (%d/%d lines)\n",h*100/t,h,t}' \
  ../coverage/coverage-integration-e2e.lcov

# Step 2: Run a single new test to verify it compiles and passes
cargo test --test resync_integration -- --nocapture

# Step 3: Run coverage for that test only (fast feedback)
cargo llvm-cov test --no-report --workspace --test resync_integration -- --nocapture
cargo llvm-cov report --lcov \
  -p contra-core -p contra-indexer -p contra-integration \
  --ignore-filename-regex '(^tests/|/test_utils/|\.cargo/registry|src/bin/|shutdown_utils\.rs|metrics\.rs|test_helpers\.rs|scheduler/greedy\.rs|core/src/client\.rs)' \
  --output-path ../coverage/coverage-integration-e2e.lcov
awk -F: '/^LF:/{t+=$2} /^LH:/{h+=$2} END{printf "Coverage: %.1f%% (%d/%d lines)\n",h*100/t,h,t}' \
  ../coverage/coverage-integration-e2e.lcov

# Step 4: Full E2E sweep
make integration-coverage
```

---

## 12. Risks & Mitigations

| Risk | Mitigation |
|------|-----------|
| `resync_integration` drops all DB tables — if parallelized could corrupt other tests | Run `resync_integration` as a standalone binary in its own nextest process (already isolated by cargo-nextest default) |
| Sender retry tests with mockito may be flaky if retries race against the mock server teardown | Use `mockito::Server` (async, not global) and pin mock expectations before starting the sender |
| `indexer_lag_integration` flaky in CI under load | Mark `#[ignore]`; run only in nightly job with `-- --ignored` |
| Node Read/Write mode tests may conflict on port if run concurrently | Each test picks a unique port via `NEXTEST_TEST_GLOBAL_SLOT` — already handled by the existing port allocation helper |
| C6 allowlist test may break if the allowlist changes | Assert on the error type/code, not the error message string, so wording changes don't break the test |
