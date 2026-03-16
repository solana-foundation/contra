# Integration Test Enhancement: Performance & Coverage Analysis

## Why Are Integration Tests Slow?

The integration test suite takes 10–20 minutes to complete. Nine root causes account
for the vast majority of that time.

---

### Issue 1 — Fresh Solana Validator Per Test Binary

**Location**: every test that calls `start_test_validator()` / `start_test_validator_no_geyser()`
(`test_utils/src/validator_helper.rs`)

**Problem**: Starting a `TestValidatorGenesis` is a blocking OS operation that takes
5–15 s per call. When each test function spins up its own validator, this cost is
paid repeatedly even if the same binary has multiple tests.

```rust
// validator_helper.rs — called once per test today
pub async fn start_test_validator() -> (TestValidator, Keypair, u16) {
    tokio::task::spawn_blocking(|| {
        let mut genesis = TestValidatorGenesis::default();
        // ... load .so files, configure geyser plugin ...
        genesis.start_with_mint_address(faucet_keypair.pubkey(), SocketAddrSpace::Unspecified)
    })
    .await
    .unwrap()
}
```

**Fix**: Share a single validator across all tests in the same binary using `OnceLock`:

```rust
static VALIDATOR: OnceLock<(TestValidator, Keypair, u16)> = OnceLock::new();

pub async fn get_or_start_test_validator() -> &'static (TestValidator, Keypair, u16) {
    if VALIDATOR.get().is_none() {
        let v = start_test_validator().await;
        let _ = VALIDATOR.set(v);
    }
    VALIDATOR.get().unwrap()
}
```

**Estimated saving**: 10–40 s per binary (one validator start eliminated per test function).

---

### Issue 2 — `SETUP_LOCK` Serializes All Contra Tests

**Location**: `integration/tests/contra/integration.rs:31`

```rust
static SETUP_LOCK: Mutex<()> = Mutex::const_new(());
```

All three test functions (`test_deposit`, `test_withdrawal`, `test_transfer`) must
acquire this lock before running. This forces strict serial execution inside the binary,
negating nextest's parallelism.

**Root cause**: `start_test_validator()` derives the gRPC port from
`NEXTEST_TEST_GLOBAL_SLOT`, which is per-process. Two concurrent calls in the same
process would collide on the same port.

**Fix**: Move each test into its own `[[test]]` binary in `Cargo.toml`. nextest runs
each binary in an isolated process, so port collisions cannot occur and the lock is
no longer needed:

```toml
[[test]]
name = "contra_deposit"
path = "tests/contra/deposit.rs"

[[test]]
name = "contra_withdrawal"
path = "tests/contra/withdrawal.rs"

[[test]]
name = "contra_transfer"
path = "tests/contra/transfer.rs"
```

**Estimated saving**: removes the serial bottleneck; all three tests run in parallel
→ wall-clock time drops from `3 × T` to `max(T)`.

---

### Issue 3 — Sequential Wallet Funding in `setup_wallets`

**Location**: `integration/tests/indexer/helpers/transactions.rs`

```rust
pub async fn setup_wallets(
    client: &RpcClient,
    faucet: &Keypair,
    wallets: &[&Keypair],
) -> Result<(), ...> {
    for wallet in wallets {
        // one RPC round-trip + confirmation per wallet
        send_and_confirm_transaction(...transfer to wallet...).await?;
        // then polls until balance is confirmed
        while client.get_balance(&wallet.pubkey()).await? < AIRDROP_AMOUNT {
            sleep(Duration::from_millis(500)).await;
        }
    }
}
```

With 5+ wallets this is 5 sequential confirmations, each taking ~2–4 s.

**Fix**: Batch all transfers into a single transaction:

```rust
pub async fn setup_wallets(
    client: &RpcClient,
    faucet: &Keypair,
    wallets: &[&Keypair],
) -> Result<(), ...> {
    let ixs: Vec<_> = wallets
        .iter()
        .map(|w| system_instruction::transfer(&faucet.pubkey(), &w.pubkey(), AIRDROP_AMOUNT))
        .collect();
    send_and_confirm_instructions(client, &ixs, faucet, &[faucet], "fund wallets").await?;
    Ok(())
}
```

**Estimated saving**: ~10–20 s across the indexer integration test (5 wallets × 2–4 s each → 1 confirmation ~2 s).

---

### Issue 4 — Sequential `mint_to_owner` Per User in `TestEnvironment::setup`

**Location**: `integration/tests/indexer/setup.rs`

```rust
for user in &users {
    // creates ATA + mints tokens — one tx per user
    mint_to_owner(client, faucet, &mint_keypair, user, initial_balance).await?;
}
```

With `NUM_USERS = 5` this is 5 sequential transactions, each requiring on-chain
confirmation.

**Fix**: Build all `create_ata_idempotent` + `mint_to` instructions in one pass and
send them in a single transaction:

```rust
let mut ixs = Vec::new();
for user in &users {
    ixs.push(create_associated_token_account_idempotent(...));
    ixs.push(spl_token::instruction::mint_to(..., initial_balance)?);
}
send_and_confirm_instructions(client, &ixs, faucet, &[faucet, &mint_authority], "batch mint").await?;
```

**Estimated saving**: ~8–15 s (4 fewer round-trips).

---

### Issue 5 — Unconditional `sleep(1s)` in `start_contra`

**Location**: `integration/tests/contra/rpc/utils.rs`

```rust
pub async fn start_contra(config: ContraConfig) -> Arc<...> {
    let node = run_node(config).await.unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await; // ← always burns 1 s
    node
}
```

This sleep is called once per test that starts a node, and 3 tests start nodes.

**Fix**: Replace with an active health-poll:

```rust
pub async fn start_contra(config: ContraConfig) -> Arc<...> {
    let node = run_node(config).await.unwrap();
    let client = RpcClient::new(format!("http://127.0.0.1:{}", config.port));
    for _ in 0..20 {
        if client.get_health().await.is_ok() { break; }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    node
}
```

**Estimated saving**: ~2–3 s total (node is usually ready in < 200 ms).

---

### Issue 6 — `SEND_AND_CHECK_DURATION_SECONDS = 1` Burns 1 s Per Sub-Test

**Location**: `integration/tests/contra/rpc/utils.rs`

```rust
const SEND_AND_CHECK_DURATION_SECONDS: u64 = 1;

pub async fn send_and_check(...) {
    // ...
    sleep(Duration::from_secs(SEND_AND_CHECK_DURATION_SECONDS)).await;
    // ...
}
```

`test_suite()` calls this helper 18 times sequentially → 18 s of pure sleep.

**Fix**: Reduce the constant to 300 ms or replace with an event-driven wait:

```rust
const SEND_AND_CHECK_DURATION_MS: u64 = 300;
```

**Estimated saving**: ~12–15 s for the `test_suite` run.

---

### Issue 7 — 500 ms Poll Interval in `wait_for_*` Helpers

**Location**: `integration/tests/indexer/helpers/db.rs` (and similar helpers)

```rust
pub async fn wait_for_count(pool: &PgPool, expected: usize, timeout_secs: u64) -> Result<bool> {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    while Instant::now() < deadline {
        let count = get_transaction_count(pool).await?;
        if count >= expected { return Ok(true); }
        sleep(Duration::from_millis(500)).await; // ← coarse poll
    }
    Ok(false)
}
```

With a 120 s timeout and 0.5 s granularity, the median extra wait is ~250 ms per
call. With many such calls across the 11-phase chaos test, this adds up.

**Fix**: Reduce to 100–200 ms:

```rust
sleep(Duration::from_millis(200)).await;
```

**Estimated saving**: 1–3 s across the full indexer test.

---

### Issue 8 — Sequential Postgres Container Startup

**Location**: `integration/tests/contra/integration.rs` (`setup()` function)

```rust
async fn setup() -> (PgPool, PgPool, PgPool) {
    let pg1 = start_postgres("contra_db").await.unwrap();    // ~3–5 s
    let pg2 = start_postgres("indexer_db").await.unwrap();   // ~3–5 s
    let pg3 = start_postgres("archive_db").await.unwrap();   // ~3–5 s
    (pg1, pg2, pg3)
}
```

Three containers start one after another, even though they are fully independent.

**Fix**: Start all three in parallel with `tokio::try_join!`:

```rust
async fn setup() -> (PgPool, PgPool, PgPool) {
    let (pg1, pg2, pg3) = tokio::try_join!(
        start_postgres("contra_db"),
        start_postgres("indexer_db"),
        start_postgres("archive_db"),
    ).unwrap();
    (pg1, pg2, pg3)
}
```

**Estimated saving**: ~6–10 s (two container startups parallelized).

---

### Issue 9 — 18-Subtest `test_suite` Runs Sequentially

**Location**: `integration/tests/contra/integration.rs` (`test_suite` function)

The 18 sub-tests are called one-by-one inside a single async function. Many of the
read-only subtests (balance queries, state checks) are independent and could run
concurrently.

**Fix**: Group the read-only assertions into a `tokio::join!` block. The mutating
tests (deposit, withdraw, transfer) must remain sequential.

```rust
// Read-only group — safe to run in parallel
tokio::join!(
    check_user_balance(&client, user1),
    check_user_balance(&client, user2),
    check_channel_state(&client),
);
```

**Estimated saving**: 3–8 s depending on how many sub-tests are parallelizable.

---

## Estimated Savings Summary

| Issue | Root Cause | Fix | Est. Saving |
|-------|-----------|-----|-------------|
| 1 | Fresh validator per test | `OnceLock` shared validator | 10–40 s |
| 2 | `SETUP_LOCK` serializes tests | Separate `[[test]]` binaries | removes `3×T` bottleneck |
| 3 | Sequential wallet funding | Batch into single transaction | 10–20 s |
| 4 | Sequential `mint_to_owner` | Batch into single transaction | 8–15 s |
| 5 | Unconditional `sleep(1s)` | Health-poll (200 ms max) | 2–3 s |
| 6 | `SEND_AND_CHECK_DURATION_SECONDS = 1` | Reduce to 300 ms | 12–15 s |
| 7 | 500 ms poll granularity | Reduce to 200 ms | 1–3 s |
| 8 | Sequential Postgres starts | `tokio::try_join!` | 6–10 s |
| 9 | 18 sequential sub-tests | Parallelize read-only group | 3–8 s |
| **Total** | | | **~52–114 s** |

Applying all fixes should reduce the full integration suite wall-clock time from
~15 minutes to under 5 minutes under nextest with default parallelism.

---

## Coverage Summary

Coverage measured from `coverage/coverage-indexer.lcov` (commit `8e44e76`).

### What IS Covered (≥ 80 %)

| Component | Coverage | Notes |
|-----------|----------|-------|
| `indexer/src/indexer/` (block stream) | ~85 % | Full chaos test exercises geyser path |
| `indexer/src/operator/processor.rs` | ~88 % | Deposit/withdrawal processing paths |
| `indexer/src/operator/sender/` | ~82 % | Mint, send, retry paths |
| `indexer/src/storage/postgres.rs` | ~90 % | All CRUD operations exercised |
| `indexer/src/operator/fetcher.rs` | ~81 % | Pending transaction polling |
| `core/src/accounts/store_block.rs` | ~85 % | Block storage and retrieval |
| `core/src/accounts/truncate.rs` | ~91 % | Apply + dry-run both covered |
| `contra_escrow_program` (client) | ~80 % | Deposit, operator setup |
| Operator idempotency (`find_existing_mint_signature`) | ~95 % | 3-scenario test |
| Gap detection / restart recovery | ~88 % | Stop/gap/restart cycle |

**Overall indexer E2E coverage: 79.0 % (9,706 / 12,280 lines)**

### What is NOT Covered (< 80 % or 0 %)

| Component | Coverage | Missing Scenarios |
|-----------|----------|------------------|
| `indexer/src/indexer/rpc_poll.rs` | ~0 % | RPC-polling datasource never exercised (only geyser path tested) |
| `indexer/src/operator/sender/state.rs` (resync) | ~35 % | In-flight resync after state divergence |
| `indexer/src/operator/alerts.rs` | ~42 % | Only happy-path alert; threshold/escalation paths missing |
| `core/src/stages/sequencer.rs` | ~61 % | Batch overflow, leader-rotation edge cases |
| `core/src/stages/executor.rs` | ~58 % | SMT proof failure, double-spend rejection |
| `core/src/stages/settler.rs` | ~55 % | Partial-batch settlement, retry-on-timeout |
| `core/src/accounts/smt.rs` | ~48 % | Tree rotation boundary (needs `--features test-tree`) |
| Fee-payer / GaslessCallback | ~20 % | Synthetic fee payer accounts never tested |
| `contra_escrow_program` (withdrawal proof verification) | ~30 % | SMT proof path only partially reached |
| Confidential Transfers (Token-2022) | ~0 % | ZK-proof path completely untested |
| Gateway routing | ~0 % | No integration test starts the gateway binary |
| Multi-node consensus / replication | ~0 % | No test covers read-replica or failover |

### Priority Gaps (Highest Coverage Impact)

1. **RPC polling path** (`rpc_poll.rs`) — single test, no hardware required, +3–5 % coverage.
2. **Executor SMT proof failure** — exercises the core safety invariant (I1/I2), +3–4 %.
3. **Settler retry-on-timeout** — covers the most common production failure mode, +2–3 %.
4. **Operator resync** (`state.rs`) — covers the crash-recovery invariant (O2), +2–3 %.
5. **Alert thresholds** — covers the monitoring invariant (O3), +1–2 %.

Addressing these five gaps would raise overall coverage from 79 % to approximately 90 %.
