# contra-bench-tps

Load testing binary for the Contra payment channel pipeline.

Generates sustained SPL token transfer load against a running Contra stack,
measures actual pipeline throughput (transactions landed per second), and
quantifies the drop rate at each concurrency level.  Controllable conflict
patterns let you stress-test the sequencer specifically or bypass it entirely
for a pure throughput baseline.

---

## Architecture

```
bench-tps/src/
├── main.rs          Entry point — wires all three phases together
├── args.rs          CLI argument definitions (clap + env vars)
├── types.rs         Shared constants, BenchConfig, BenchState, BatchQueue
├── setup.rs         Phase 1 — on-chain setup (mint, ATAs, initial balances)
├── background.rs    Phase 2 — blockhash poller + metrics sampler
├── load.rs          Phase 3 — transaction generator + sender threads
└── rpc.rs           Helpers — send_parallel, poll_confirmations
```

### Phase 1 — Setup

Runs once before load begins. All steps must complete and confirm on-chain
before the load phase starts.

| Step | What happens |
|------|-------------|
| 1 | Load admin keypair from `--admin-keypair` JSON file |
| 2 | Generate `--accounts` fresh keypairs in parallel (rayon) |
| 3 | Create SPL mint via `initialize_mint` + retry backoff |
| 4 | Create ATAs for every keypair in parallel (chunks of 64) |
| 5 | Poll `getSignatureStatuses` until all ATAs are confirmed |
| 6 | Mint `--initial-balance` tokens to every ATA in parallel |
| 7 | Poll until all mint-to transactions are confirmed |
| 8 | Fetch current blockhash → seed `BenchState` |

**Why `send_transaction` + `poll_confirmations` instead of `send_and_confirm_transaction`?**
The Contra node settles asynchronously through its pipeline. The built-in
client timeout (tied to blockhash expiry, ~60 s) fires before settlement
completes. The custom poller uses a 120 s deadline and 500 ms intervals.

### Phase 2 — Background tasks (run concurrently with Phase 3)

**Blockhash poller** — calls `getLatestBlockhash` every 80 ms and writes the
result into `BenchState::current_blockhash`.  The node's dedup stage rejects
transactions whose blockhash is older than ~15 s; 80 ms keeps the bench well
within that window.  On RPC error the cached hash is kept (still valid for ~15 s).

**Metrics sampler** — calls `getTransactionCount` every second using
`CommitmentConfig::processed()` (most up-to-date view).  Diffs successive
values to produce instantaneous TPS.  Returns `(start_count, end_count)` for
the final drop-rate summary.

### Phase 3 — Load generation

```
┌──────────────────────────────────────┐
│  Generator task (async, tokio)       │  reads BenchState::current_blockhash
│  signs batch of N transactions       │  cycles through accounts/destinations
│  push Vec<Transaction> → BatchQueue  │  yields if queue depth >= 32
└──────────────────────┬───────────────┘
                       │ Mutex + Condvar
          ┌────────────┼──────────────┐
          ▼            ▼              ▼
   Sender thread 0  Sender thread 1  ...  (--threads OS threads)
   pop batch        pop batch
   send_transaction (blocking RpcClient)
   sent_count += batch.len()
   sleep --sender-sleep-ms
```

**Generator** (async tokio task): signs batches of `--threads` transactions.
Cycles through source keypairs and destination wallets using a wrapping counter
so no two consecutive batches reuse the same pair.  Applies backpressure by
yielding when the queue depth reaches `MAX_QUEUE_DEPTH = 32`.

**Sender threads** (OS threads, blocking): each owns its own
`solana_client::rpc_client::RpcClient` (no lock contention).  Waits on a
condvar with a 50 ms timeout (so cancellation is checked promptly even when
idle).  Calls the synchronous `send_transaction` for each transaction in the
batch — the blocking call naturally throttles the sender to one RPC round-trip
per transaction, giving the generator time to pre-sign the next batch.

**Conflict groups** — `--num-conflict-groups` controls how many distinct
destination ATAs the generator uses:
- `1` → all transfers go to the same destination → every transaction conflicts
  → sequencer must serialise them → maximum sequencer pressure
- `== accounts` (default) → each source sends to a unique destination →
  no conflicts → sequencer can parallelise freely → maximum throughput

### Final summary

```
sent    — transactions dispatched by sender threads (AtomicU64 counter)
landed  — getTransactionCount delta over the load window (node's own counter)
dropped — sent - landed (rejected by dedup / sigverify / sequencer / network)
drop_rate — dropped / sent as a percentage
tps     — landed / duration_secs
```

---

## Build

```bash
# Build the release binary (from repo root or bench-tps/)
cargo build --release --manifest-path bench-tps/Cargo.toml

# Binary lands at:
bench-tps/target/release/contra-bench-tps
```

---

## Run

### Quick start (via run.sh)

`scripts/run.sh` handles everything: generates the admin keypair, builds
Docker images if needed, starts all services, waits for them to stabilise,
then runs the bench.

```bash
# First time setup — copy and edit the env file
cp bench-tps/.env.sample bench-tps/.env
# (edit bench-tps/.env — defaults work for local runs)

# Build binary first
cargo build --release --manifest-path bench-tps/Cargo.toml

# Run with defaults (50 accounts, 4 threads, 60 s)
cd contra/
./bench-tps/scripts/run.sh

# Force-rebuild images and regenerate keypair
./bench-tps/scripts/run.sh --rebuild

# Wipe corrupt data volumes and start clean
./bench-tps/scripts/run.sh --clean

# Pass custom bench args (everything after script flags is forwarded)
./bench-tps/scripts/run.sh --threads 20 --duration 120

# Maximum sequencer contention (all senders target one destination)
./bench-tps/scripts/run.sh --num-conflict-groups 1 --threads 20 --duration 60
```

### Manual run (binary only, services already running)

```bash
./bench-tps/target/release/contra-bench-tps \
  --admin-keypair ./bench-tps/admin-keypair.json \
  --rpc-url http://localhost:8898 \
  --accounts 50 \
  --threads 8 \
  --duration 120
```

---

## Arguments

| Flag | Env var | Default | Description |
|------|---------|---------|-------------|
| `--admin-keypair` | `BENCH_ADMIN_KEYPAIR` | — | Path to admin keypair JSON (generated by run.sh) |
| `--rpc-url` | `BENCH_RPC_URL` | `http://localhost:8899` | Write-node or gateway endpoint |
| `--accounts` | `BENCH_ACCOUNTS` | `50` | Number of funded source accounts |
| `--duration` | `BENCH_DURATION` | `60` | Load phase duration in seconds |
| `--threads` | `BENCH_THREADS` | `4` | Number of concurrent sender threads |
| `--num-conflict-groups` | `BENCH_NUM_CONFLICT_GROUPS` | `== accounts` | Distinct destination accounts (1 = max contention) |
| `--initial-balance` | `BENCH_INITIAL_BALANCE` | `1_000_000` | Raw token units minted per account |
| `--sender-sleep-ms` | `BENCH_SENDER_SLEEP_MS` | `10` | Sleep per sender thread after each batch (0 = no sleep) |
| `--metrics-port` | `BENCH_METRICS_PORT` | — | Optional Prometheus `/metrics` port |
| `--log-level` | `BENCH_LOG_LEVEL` | `info` | Tracing log level (`RUST_LOG` takes precedence) |

**`--accounts` must be >= `--threads`** to avoid multiple senders sharing a
keypair, which would cause nonce conflicts and failed transactions.

---

## Log interpretation

### Setup phase

```
INFO Loaded admin keypair pubkey=... path=...
INFO Generated account keypairs count=50 elapsed_ms=12
INFO Mint initialized mint=... elapsed_ms=1840
INFO ATA transactions sent sent=50 total=50 elapsed_ms=430
INFO ATAs confirmed confirmed=50 elapsed_ms=2100
INFO Mint-to transactions sent sent=50 total=50 elapsed_ms=380
INFO Mint-to confirmed confirmed=50 elapsed_ms=1950
INFO Initial blockhash seeded — setup complete blockhash=... elapsed_ms=15
```

Long `elapsed_ms` on `Mint initialized` or confirmations is normal — the
Contra pipeline settles asynchronously and may take 1–3 s per batch.

### Load phase (repeating every second)

```
INFO metrics tps=312 total_tx=18720 remaining_secs=47
INFO blockhash_poller avg fetch latency fetches=25 avg_fetch_us=840
```

- `tps` — transactions that landed at the node in the last second
- `total_tx` — cumulative node transaction count since startup
- `remaining_secs` — seconds left in the load phase
- `avg_fetch_us` — average blockhash fetch round-trip in microseconds;
  values consistently > 5000 µs suggest network latency to the node

### Final summary

```
INFO Final summary duration_secs=60 sent=18900 landed=18540 dropped=360 drop_rate=1.9% tps=309.0
```

- **`sent`** — total transactions the bench dispatched
- **`landed`** — transactions confirmed by the node (getTransactionCount delta)
- **`dropped`** — transactions that did not land (`sent - landed`)
- **`drop_rate`** — `dropped / sent` × 100; under 5% is healthy; consistently
  above 10% indicates pipeline back-pressure (dedup saturated, sigverify
  queue full, or sequencer overwhelmed)
- **`tps`** — effective pipeline throughput over the entire run duration

### Common warnings

```
WARN initialize_mint send failed, retrying in 2s attempt=0 err=...502 Bad Gateway...
```
Write-node not yet ready; the retry loop handles this automatically.

```
WARN blockhash_poller: get_latest_blockhash failed, keeping cached hash err=...
```
Transient RPC error; safe to ignore as long as it is infrequent (cached hash
is valid for ~15 s).

```
WARN sender: send_transaction failed err=...
```
Individual transaction rejected — increments `dropped`.  Occasional failures
are expected; a high rate points to dedup (stale blockhash), sigverify
(invalid signature), or the node being overloaded.

---

## Workspace isolation

**`bench-tps` is intentionally excluded from the root Cargo workspace.**

```toml
# Cargo.toml (repo root)
[workspace]
exclude = ["bench-tps"]
```

### Why

The Contra workspace pins service dependencies tightly — specific versions of
`solana-client`, `tokio`, `sqlx`, etc. — because those versions must be
ABI-compatible with the Solana validator and postgres driver at runtime.

`bench-tps` only sends HTTP requests; it does not link against the same native
libraries and does not need to match those pins exactly.  Keeping it excluded:

1. **Prevents dependency hell**: adding bench-specific crates (e.g. heavier
   testing utilities) cannot break the workspace-wide dependency resolution.
2. **Faster iteration**: `cargo build` inside `bench-tps/` only rebuilds the
   bench crate and its direct deps, not the entire workspace.
3. **Independent lock file**: `bench-tps/Cargo.lock` tracks the bench's own
   resolved dependency graph.  Changes to workspace deps do not force a bench
   lockfile update, and vice versa.

The bench still depends on `contra-core` via a path dependency and inherits
whatever version of `solana-sdk` / `solana-client` that crate uses.
