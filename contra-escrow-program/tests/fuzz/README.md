# Escrow Program Fuzz Tests

## Why fuzz this program

The escrow program's security rests on two properties that are hard to verify by inspection alone:

1. **No double-spend.** Once a nonce is used to release funds, it must be permanently rejected. This is enforced by an on-chain Sparse Merkle Tree (SMT): every successful release transitions the root from one that excludes the nonce to one that includes it, making a future exclusion proof for the same nonce impossible to construct. A bug here means an operator could drain the escrow by replaying a release.

2. **Balance conservation.** The total deposited into the escrow must always equal what is still held plus what was legitimately released. Tokens must not appear or disappear as a side-effect of any sequence of operations.

Both properties must hold across arbitrary operation sequences — not just the happy path. Fuzzing generates thousands of such sequences automatically, including edge cases like interleaved deposits and releases, repeated releases of the same nonce, and SMT tree resets between operations.

The integration tests cover correctness of individual instructions. These harnesses cover **compositional correctness**: invariants that can only break when operations interact in unexpected ways.

## Harnesses

| Binary | What it covers |
|---|---|
| `fuzz_escrow` | Core deposit → release → double-spend lifecycle. Verifies that valid SMT proofs succeed, invalid/replayed proofs are rejected, and balances are conserved across all combinations. |
| `fuzz_reset_smt` | SMT reset lifecycle. Verifies that nonces from a previous tree generation are permanently rejected after a reset, and that balances are conserved across any number of resets. |


## Prerequisites

```
cargo install honggfuzz
```

The program binary must be built with **production settings** before running any harness. If the binary was last built with `make build-test` (the `test-tree` feature), the harnesses will fail because they assume `MAX_TREE_LEAVES = 65536`. See [Gotchas](#gotchas).

```
cd contra-escrow-program && make build
```

## Running

From `contra-escrow-program/tests/fuzz/`:

```
HFUZZ_RUN_ARGS="-n 4 -t 30" cargo hfuzz run fuzz_escrow
HFUZZ_RUN_ARGS="-n 4 -t 30" cargo hfuzz run fuzz_reset_smt
```

Useful flags:

| Flag | Meaning |
|---|---|
| `-n <N>` | Number of threads |
| `-t <S>` | Per-iteration timeout in seconds (default: 1 — too short for LiteSVM, use 30) |
| `--exit_upon_crash` | Stop as soon as a crash is found |

## Debugging crashes

Honggfuzz saves crash inputs to `hfuzz_workspace/<target>/` as `.fuzz` files. It does **not** print the panic message to the terminal — the process output is swallowed by the persistent-mode runtime.

Each harness has a `#[cfg(test)] replay` test that reads a crash file and runs the exact same logic under `cargo test`, where panics print normally.

**Step 1 — confirm the binary is fresh:**

```
cd contra-escrow-program && make build
```

**Step 2 — replay the crash:**

```
cd contra-escrow-program/tests/fuzz
export CRASH_FILE=$(ls hfuzz_workspace/fuzz_reset_smt/*.fuzz | head -1)
RUST_BACKTRACE=1 cargo test --bin fuzz_reset_smt replay -- --nocapture
```

Replace `fuzz_reset_smt` with `fuzz_escrow` as needed. The full panic message and program logs will print to stdout.

## Gotchas

### 1. Wrong program binary (`test-tree` feature)

The program has two build modes:

| Build | Feature | `MAX_TREE_LEAVES` | `TREE_HEIGHT` | Command |
|---|---|---|---|---|
| Production | _(none)_ | 65,536 | 16 | `make build` |
| Test | `test-tree` | 8 | 3 | `make build-test` |

The fuzz harnesses always assume the production binary. If `target/deploy/contra_escrow_program.so` was built with `make build-test`, nonces above 7 will hit `InvalidTransactionNonceForCurrentTreeIndex` (error 13) and the harness will panic.

**Symptom:** Crash immediately on iterations with nonce > 7, replaying shows `Custom(13)`.

**Fix:** `cd contra-escrow-program && make build`

### 2. Compute unit limit

The production binary (TREE_HEIGHT=16) uses up to ~1.2M compute units per `ReleaseFunds` call. LiteSVM's default limit is 200k. Any release that reaches SMT proof verification must be sent with an explicit CU budget.

**Symptom:** `ProgramFailedToComplete` / `exceeded CUs meter at BPF instruction`.

**Fix:** Already applied in both harnesses. If you add new release calls that reach proof verification, use `send_transaction_with_signers_with_transaction_result` with `Some(1_200_000)`. Calls that are rejected before proof verification (e.g. stale nonce range check) don't need it and can use `send_transaction_with_signers`.

### 3. Stale crash files

Honggfuzz skips crash files that already exist in the workspace (`Crash (dup): ... skipping`). If the binary was rebuilt and you want to verify a previously crashing input no longer crashes, use the replay test above rather than re-running the fuzzer. To get fresh crash discovery, delete the old files:

```
rm hfuzz_workspace/fuzz_reset_smt/*.fuzz
```

### 4. `AlreadyProcessed` on repeated operations

LiteSVM deduplicates transactions by signature. Two transactions with identical instruction data, accounts, and blockhash produce the same signature — the second is rejected with `AlreadyProcessed`. This surfaces as a panic in the `Deposit` handler when the fuzzer generates repeated ops with the same parameters.

**Symptom:** `Transaction failed: AlreadyProcessed` inside `assert_get_or_deposit`.

**Fix:** Already applied in both harnesses — `warp_to_slot` is called once per loop iteration, which calls `expire_blockhash()` internally and ensures every transaction in the sequence gets a unique signature. If you add a new harness, include this at the top of the op loop:

```rust
for (slot, op) in input.ops.into_iter().take(32).enumerate() {
    ctx.test_context.warp_to_slot(slot as u64 + 2);
    ...
}
```

### 5. Timeout of 1 second (default)

The default honggfuzz timeout is 1 second. LiteSVM + SMT operations take longer. Always pass `-t 30`:

```
HFUZZ_RUN_ARGS="-n 1 -t 30 --exit_upon_crash" cargo hfuzz run fuzz_reset_smt
```
