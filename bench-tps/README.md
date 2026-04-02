# contra-bench-tps

Load testing binary for the Contra payment channel. Supports three flows:

| Flow | What it stresses |
|------|-----------------|
| `transfer` | L2 SPL token transfer pipeline (dedup → sigverify → sequencer → executor → settler) |
| `deposit` | L1 escrow deposits (Solana → contra, measured end-to-end via operator-solana) |
| `withdraw` | L2 burn + L1 release (contra → Solana, measured end-to-end via operator-contra) |

---

## Quick start

```bash
# 1. Copy env file and set values (defaults work for local runs)
cp bench-tps/.env.sample bench-tps/.env

# 2. Build (from repo root)
cargo build --release -p contra-bench-tps
# Binary: target/release/contra-bench-tps

# 3. Run (from repo root)
./bench-tps/scripts/run.sh                        # transfer flow, defaults
./bench-tps/scripts/run.sh deposit                # deposit flow
./bench-tps/scripts/run.sh withdraw               # withdraw flow
./bench-tps/scripts/run.sh --rebuild              # force-rebuild images + keypair
./bench-tps/scripts/run.sh --clean                # wipe volumes, start fresh
```

`run.sh` handles everything: generates the admin keypair, starts the full Docker
stack, waits for services to stabilise, runs the bench, then tears down all
containers on exit.

---

## Transfer flow

### What it does

Generates sustained L2 SPL token transfers against the Contra write-node
(via the gateway).  Each sender thread cycles through funded source accounts,
signing a unique transfer + memo instruction per transaction.

### How to run

```bash
./bench-tps/scripts/run.sh \
    --accounts 500 \
    --threads 8 \
    --duration 120

# Maximum sequencer contention (all senders target one destination)
./bench-tps/scripts/run.sh --num-conflict-groups 1 --threads 20
```

### What it measures

| Field | Source | Meaning |
|-------|--------|---------|
| `sent` | `AtomicU64` counter | Transactions dispatched by sender threads |
| `landed` | `getTransactionCount` delta | Transactions confirmed by the node |
| `dropped` | `sent - landed` | Rejected by dedup / sigverify / sequencer / network |
| `tps` | `landed / duration` | Effective pipeline throughput |

### Config parameters

| Flag | Env var | Default | Notes |
|------|---------|---------|-------|
| `--rpc-url` | `BENCH_RPC_URL` | `http://localhost:8898` | L2 gateway endpoint |
| `--accounts` | `BENCH_ACCOUNTS` | `50` | Source keypairs; must be ≥ `--threads` |
| `--duration` | `BENCH_DURATION` | `60` | Load phase seconds |
| `--threads` | `BENCH_THREADS` | `4` | Concurrent sender threads |
| `--num-conflict-groups` | `BENCH_NUM_CONFLICT_GROUPS` | `== accounts` | Distinct destination ATAs (1 = max contention) |
| `--initial-balance` | `BENCH_INITIAL_BALANCE` | `1_000_000` | Raw token units per account |
| `--sender-sleep-ms` | `BENCH_SENDER_SLEEP_MS` | `5` | Throttle per-thread (0 = max throughput) |

---

## Deposit flow

### What it does

Sends L1 `Deposit` instructions to the Solana validator's escrow program.
Each transaction transfers tokens from a depositor's L1 ATA into the shared
escrow instance ATA.  The full e2e path ends when `operator-solana` picks up
the indexed deposit and mints an equivalent amount on L2.

```
bench → L1 Deposit → indexer-solana indexes event
      → operator-solana mints on L2
```

### How to run

```bash
./bench-tps/scripts/run.sh deposit \
    --accounts 500 \
    --threads 8 \
    --duration 120
```

### What it measures

| Field | Source | Meaning |
|-------|--------|---------|
| `sent` | `AtomicU64` | Deposit txs dispatched |
| `l1_landed` | `getTransactionCount` delta on L1 | Confirmed by validator |
| `l2_minted` | `contra_operator_mints_sent_total{escrow}` delta | L2 mints confirmed by operator |
| `drop` | `l1_landed - l2_minted` | Deposits landed but not yet minted (indexer/operator lag) |

`l2_minted` requires `BENCH_OPERATOR_METRICS_URL` to be set (operator-solana
exposes metrics on port 9102).

### Config parameters

| Flag | Env var | Default | Notes |
|------|---------|---------|-------|
| `--l1-rpc-url` | `BENCH_L1_RPC_URL` | `http://localhost:18899` | L1 validator endpoint |
| `--accounts` | `BENCH_DEPOSIT_ACCOUNTS` | `20` | Depositor keypairs |
| `--duration` | `BENCH_DURATION` | `60` | Load phase seconds |
| `--threads` | `BENCH_THREADS` | `4` | Concurrent sender threads |
| `--initial-balance` | `BENCH_INITIAL_BALANCE` | `1_000_000` | L1 token units per account |
| `--operator-metrics-url` | `BENCH_OPERATOR_METRICS_URL` | — | `http://localhost:9102/metrics` for e2e tracking |
| `--instance-seed-keypair` | `BENCH_INSTANCE_SEED_KEYPAIR` | — | Reuse persistent escrow instance across runs |

---

## Withdraw flow

### What it does

The most complex flow — exercises the full cross-chain withdrawal path:

```
bench → L2 WithdrawFunds (burn) → indexer-contra indexes event
      → operator-contra sends L1 ReleaseFunds → funds released on Solana
```

Setup creates both L1 and L2 state: an escrow instance on Solana, L2 ATAs
funded with tokens, and L1 ATAs so that `ReleaseFunds` can transfer to them.

### How to run

```bash
./bench-tps/scripts/run.sh withdraw \
    --accounts 500 \
    --threads 8 \
    --duration 120
```

### What it measures

| Field | Source | Meaning |
|-------|--------|---------|
| `sent` | `AtomicU64` | WithdrawFunds txs dispatched |
| `l2_burned` | `getTransactionCount` delta on L2 | Burns confirmed by the write-node |
| `l1_released` | `contra_operator_mints_sent_total{withdraw}` delta | L1 ReleaseFunds confirmed by operator |
| `drop` | `l2_burned - l1_released` | Burns not yet released (indexer/operator lag) |

`l1_released` requires `BENCH_WITHDRAW_OPERATOR_METRICS_URL` (operator-contra
on port 9103).

### Config parameters

| Flag | Env var | Default | Notes |
|------|---------|---------|-------|
| `--rpc-url` | `BENCH_RPC_URL` | `http://localhost:8898` | L2 gateway endpoint |
| `--l1-rpc-url` | `BENCH_L1_RPC_URL` | `http://localhost:18899` | L1 validator endpoint |
| `--accounts` | `BENCH_WITHDRAW_ACCOUNTS` | `20` | Withdrawer keypairs |
| `--duration` | `BENCH_DURATION` | `60` | Load phase seconds |
| `--threads` | `BENCH_THREADS` | `4` | Concurrent sender threads |
| `--initial-balance` | `BENCH_INITIAL_BALANCE` | `1_000_000` | L2 token units per account |
| `--operator-metrics-url` | `BENCH_WITHDRAW_OPERATOR_METRICS_URL` | — | `http://localhost:9103/metrics` for e2e tracking |
| `--instance-seed-keypair` | `BENCH_INSTANCE_SEED_KEYPAIR` | — | Must match `COMMON_ESCROW_INSTANCE_ID` in docker-compose |

---

## Log interpretation

### Setup phase

```
INFO Loaded admin keypair pubkey=... path=...
INFO Generated account keypairs count=500 elapsed_ms=12
INFO Mint initialized mint=... elapsed_ms=1840
INFO ATAs confirmed confirmed=500 elapsed_ms=2100
INFO Mint-to confirmed confirmed=500 elapsed_ms=1950
INFO Initial blockhash seeded — setup complete
```

Long `elapsed_ms` on confirmations is normal — the Contra pipeline settles
asynchronously (1–3 s per batch).

### Load phase (logged every second)

```
INFO metrics tps=312 total_tx=18720 remaining_secs=47
INFO operator confirmed/s confirmed_per_sec=8 total_confirmed=240 program_type=withdraw
INFO blockhash_poller avg fetch latency fetches=25 avg_fetch_us=840
```

### Final summary

**Transfer:**
```
INFO Final summary duration_secs=60 sent=18900 landed=18540 dropped=360 drop_rate=1.9% tps=309.0
```

**Deposit:**
```
INFO Final summary duration_secs=60 sent=30000 l1_landed=28500 l2_minted=280 drop=28220 drop_rate=99.0% l1_tps=475.0 l2_tps=4.7
```
> High `drop` between `l1_landed` and `l2_minted` is expected during the run — the operator
> pipeline has latency. Re-run with a longer `--duration` to reach steady state.

**Withdraw:**
```
INFO Final summary duration_secs=60 sent=3000 l2_burned=2940 l1_released=21 drop=2919 drop_rate=99.3% l2_tps=49.0 l1_tps=0.4
```

### Common warnings

```
WARN initialize_mint send failed, retrying in 2s attempt=0 err=...502...
```
Write-node not ready — retry loop handles this automatically.

```
WARN blockhash_poller: get_latest_blockhash failed, keeping cached hash
```
Transient RPC error; safe to ignore if infrequent (cached hash valid ~15 s).

```
WARN sender: send_transaction failed err=...
```
Individual transaction rejected — increments `dropped`. Occasional failures are
expected; a high rate points to dedup (stale blockhash) or the node being
overloaded.


## Architecture

```
bench-tps/src/
├── main.rs            Entry point — dispatches to run_transfer / run_deposit / run_withdraw
├── args.rs            CLI argument definitions (clap + env vars)
├── types.rs           Shared constants, BenchConfig, BenchState, BatchQueue
├── setup.rs           Transfer setup — mint, ATAs, balances
├── setup_deposit.rs   Deposit setup — escrow instance, depositor accounts
├── setup_withdraw.rs  Withdraw setup — escrow instance, L2 accounts, L1 ATAs
├── background.rs      Blockhash poller, metrics sampler, operator mints sampler
├── load.rs            Transfer generator + sender threads
├── load_deposit.rs    Deposit generator + sender threads
├── load_withdraw.rs   Withdraw generator + sender threads
└── rpc.rs             Helpers — send_parallel, poll_confirmations
```

### Three-phase structure (all flows)

**Phase 1 — Setup**: creates all on-chain state before load begins.

**Phase 2 — Background tasks** (concurrent with Phase 3):
- **Blockhash poller** — refreshes `BenchState::current_blockhash` every 80 ms.
  The dedup stage rejects transactions with a blockhash older than ~15 s.
- **Metrics sampler** — polls `getTransactionCount` every second to compute
  landed TPS and returns `(start, end)` counts for the final summary.
- **Operator mints sampler** (deposit/withdraw only) — scrapes
  `contra_operator_mints_sent_total` from the operator Prometheus endpoint
  every second for e2e confirmation tracking.

**Phase 3 — Load generation**:
```
Generator (async tokio task)
  reads current_blockhash → signs batch → push to BatchQueue
  yields if queue depth ≥ 32 (backpressure)
        │
        └─ BatchQueue (Mutex<VecDeque> + Condvar)
              │
        ┌─────┴──────┐
    Sender 0      Sender N   (OS threads, --threads count)
    pop batch → send_transaction (blocking RpcClient)
    sent_count += batch.len()
    sleep --sender-sleep-ms
```

### Uniqueness guarantee

Every transaction includes a **memo instruction** encoding the monotonically
increasing `tx_seq` counter as its data.  This ensures every transaction has a
unique signature regardless of account or blockhash reuse, preventing the
dedup stage from dropping them as duplicates.

### Binary location

`bench-tps` is a Cargo workspace member.  Cargo always writes the binary to
the workspace root target directory:

```
target/release/contra-bench-tps   ← correct
bench-tps/target/                  ← does not exist
```

Build command:
```bash
cargo build --release -p contra-bench-tps
```

---

## CPU pinning verification

```bash
# Container CPU sets
docker ps --filter "name=contra-" --format "{{.Names}}" \
  | xargs -I{} docker inspect --format '{{.Name}} {{.HostConfig.CpusetCpus}}' {}

# Bench process CPU set
taskset -pc $(pgrep -f contra-bench-tps)
```
