# Trident Fuzz Tests — contra-escrow-program

Stateful fuzz tests for the escrow program using [Trident](https://github.com/Ackee-Blockchain/trident) v0.12.0.

## Harnesses

### `fuzz_escrow`

Tests the core escrow lifecycle in a single tree generation.

| Flow                | Description                                                                                  |
| ------------------- | -------------------------------------------------------------------------------------------- |
| `fuzz_deposit`      | Deposits a random amount. Asserts exact ATA balance movement.                                |
| `fuzz_release`      | 50% valid proof release / 50% garbage proof. Asserts success/failure and balance invariants. |
| `fuzz_double_spend` | Replays a previously successful release with the exact same proof — must always be rejected. |

**Final invariant:** `escrow_balance == total_deposited - total_released`

### `fuzz_reset_smt`

Tests the SMT reset lifecycle across multiple tree generations.

| Flow                  | Description                                                                                         |
| --------------------- | --------------------------------------------------------------------------------------------------- |
| `fuzz_deposit`        | Deposits a random amount.                                                                           |
| `fuzz_release`        | Valid release within the current tree generation. Skipped silently if preconditions aren't met.     |
| `fuzz_reset_smt_root` | Resets the on-chain SMT root, advancing the tree generation index. Asserts balances are unaffected. |
| `fuzz_stale_nonce`    | Attempts a release with a nonce from the previous generation — must always be rejected.             |

**Final invariant:** `escrow_balance == total_deposited - total_released`

## Running

Build the program first (from repo root):

```bash
cargo build-sbf
```

Run a harness:

```bash
cd contra-escrow-program/tests/trident-tests
cargo run --bin fuzz_escrow
cargo run --bin fuzz_reset_smt
```

Debug mode — single-threaded, panics and program logs visible:

```bash
cargo build --bin fuzz_escrow
TRIDENT_FUZZ_DEBUG=0000000000000000 ./target/debug/fuzz_escrow 2>&1 | head -200
```

## Structure

```
trident-tests/
  shared.rs         # Shared constants, AccountAddresses, setup_escrow, token_amount
  fuzz_escrow.rs    # Core lifecycle harness
  fuzz_reset_smt.rs # SMT reset lifecycle harness
  Cargo.toml
  Trident.toml
```

## Notes

- The Pinocchio program uses `sol_get_sysvar` for `Rent::get()`, which requires patched Trident syscall stubs. See `[patch.crates-io]` in `Cargo.toml`.
- `ReleaseFunds` requires 1.2M compute units for SMT proof verification — all release transactions include a `ComputeBudgetInstruction`.
- Nonces in `fuzz_reset_smt` are generation-aware: `nonce = tree_index * MAX_TREE_LEAVES + offset` to ensure they belong to the current tree.
