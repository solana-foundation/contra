# contra-bench-tps: Rust Load Testing Binary

## Context

Goal: identify bottlenecks in contra's 5-stage pipeline (Dedup → Sigverify → Sequencer → Executor → Settler) by generating realistic sustained load with SPL token transfer transactions, including controllable account conflict patterns.

**Why not k6**: k6 runs in a goja (JS) runtime — not Node.js. It has no Ed25519 signing capability, cannot sign Solana transactions at runtime, cannot handle blockhash refresh (dedup rejects hashes older than ~15s), and cannot run the setup/funding phase. k6 is the wrong tool for this.

**Why a Rust binary**: The existing codebase already has all the building blocks (`client.rs` transaction builders, `solana-client` RPC, `contra-metrics`). A binary modeled after agave/bench-tps slots naturally into `core/src/bin/` alongside `node.rs` and `activity.rs`.

---

## Files to Create / Modify

| File | Change |
|------|--------|
| `core/src/bin/bench_tps.rs` | **New file** — the entire binary |
| `core/Cargo.toml` | Add `contra-metrics` dependency + `[[bin]]` entry |

---

## Existing Code to Reuse Directly

All from `core/src/client.rs`:
- `create_spl_transfer(from, to, mint, amount, blockhash) -> Transaction`
- `create_ata_transaction(payer, owner, mint, blockhash) -> Transaction`
- `create_admin_initialize_mint(admin, mint, decimals, blockhash) -> Transaction`
- `create_admin_mint_to(admin, mint, destination, amount, blockhash) -> Transaction`
- `load_keypair(path) -> Result<Keypair>`

From `metrics/src/lib.rs`:
- `start_metrics_server(port: u16)` — spawns axum `/metrics` endpoint
- `counter_vec!`, `histogram_vec!`, `gauge_vec!`, `init_metrics!` macros

Pattern from `core/src/bin/activity.rs`:
- `RpcClient::new(url)` per task
- `get_latest_blockhash().await`
- `send_transaction(&tx).await`
- `tokio::spawn` for concurrent tasks
- `CancellationToken` for coordinated shutdown

---

## Cargo.toml Changes (`core/Cargo.toml`)

```toml
# Add to [dependencies]:
contra-metrics = { path = "../metrics" }

# Add new [[bin]] section (for custom binary name):
[[bin]]
name = "contra-bench-tps"
path = "src/bin/bench_tps.rs"
```

All other required crates (`solana-client`, `solana-sdk`, `spl-token`, `spl-associated-token-account`, `tokio`, `clap`, `tracing`, `anyhow`, `rand`) are already present.

---

## Binary Structure

### Phase 1: Setup
1. Load admin keypair from `--admin-keypair` JSON file via `load_keypair()`
2. Generate N keypairs (`--accounts`, default 50)
3. Create SPL mint via `create_admin_initialize_mint()` + `send_and_confirm_transaction()`
4. For each keypair in batches of 20: `create_ata_transaction()` (fire-and-forget)
5. Sleep 500ms for ATA confirmations
6. For each keypair in batches of 20: `create_admin_mint_to()` (fire-and-forget)
7. Sleep 1000ms for mint-to confirmations
8. Fetch initial blockhash → seed `Arc<RwLock<Hash>>`

> Note: `create_admin_initialize_mint` emits only `initialize_mint` (no `create_account`). This works against the contra node (which handles account creation internally) but would fail on a raw Solana cluster.

### Phase 2: Background Tasks
- **Blockhash poller**: `tokio::time::interval(80ms)`, calls `get_latest_blockhash()`, writes to `Arc<RwLock<Hash>>`. On error, keeps old hash (safe for up to 15s).
- **Metrics sampler**: Every 5s, reads atomic counters, computes instantaneous TPS, logs to stdout + updates `BENCH_TPS_CURRENT` gauge.

### Phase 3: Sender Tasks (`--threads`)
Each task loops until `duration` elapsed or `CancellationToken` cancelled:
1. Read blockhash from `Arc<RwLock<Hash>>` (cheap read lock)
2. Pick source keypair: `keypairs[task_id % num_accounts]`
3. Pick destination ATA: `destinations[task_id % num_conflict_groups]`
4. Call `create_spl_transfer()` → sign tx
5. `rpc_client.send_transaction(&tx).await` (returns after HTTP response)
6. On success: increment `success_total`, record RTT in histogram
7. On error: increment `failed_total`, log warn
8. `tokio::task::yield_now().await`

Each sender task creates its own `RpcClient` (independent connection pool, no lock contention).

### Phase 4: Final Report
Print: total sent/success/failed, overall TPS, success rate %, RTT p50/p95/p99.

---

## Key Data Structures

```rust
struct Args {
    rpc_url: String,                    // default: http://localhost:8899
    admin_keypair: PathBuf,
    accounts: usize,                    // default: 50
    duration: u64,                      // seconds, default: 60
    threads: usize,                     // default: 10
    num_conflict_groups: Option<usize>, // default = accounts (no conflict)
    initial_balance: u64,               // tokens per account, default: 1_000_000
    metrics_port: Option<u16>,          // optional Prometheus endpoint
    log_level: String,                  // default: info
}

// Immutable config shared across all sender tasks
struct BenchConfig {
    rpc_url: String,
    mint: Pubkey,
    accounts: Vec<Arc<Keypair>>,
    destinations: Vec<Pubkey>,  // len = num_conflict_groups
    duration_secs: u64,
}

// Mutable shared state (all atomic or RwLock)
struct BenchState {
    current_blockhash: RwLock<Hash>,
    sent_total: AtomicU64,
    success_total: AtomicU64,
    failed_total: AtomicU64,
}
```

---

## Conflict Group Logic

```rust
// Build destination ATAs using first N accounts (already have ATAs from setup)
fn build_destinations(accounts: &[Arc<Keypair>], mint: &Pubkey, n: usize) -> Vec<Pubkey> {
    accounts.iter().take(n)
        .map(|kp| get_associated_token_address(&kp.pubkey(), mint))
        .collect()
}

// Pick destination by task_id
fn pick_destination(task_id: usize, destinations: &[Pubkey]) -> Pubkey {
    destinations[task_id % destinations.len()]
}
```

- `--num-conflict-groups 1`: all senders write to same destination ATA → max sequencer contention (all txs in separate conflict-free batches)
- `--num-conflict-groups N` (N = accounts): each sender writes to unique destination → no conflicts (max throughput through sequencer)

**Requirement**: `--accounts >= --threads` to avoid keypair sharing across tasks.

---

## Metrics

Defined as module-level statics using `contra-metrics` macros:

```rust
counter_vec!(BENCH_SENT_TOTAL,    "contra_bench_tps_sent_total",    "Total transactions sent",              &[]);
counter_vec!(BENCH_SUCCESS_TOTAL, "contra_bench_tps_success_total", "Transactions accepted by node",        &[]);
counter_vec!(BENCH_FAILED_TOTAL,  "contra_bench_tps_failed_total",  "Transactions rejected or errored",     &[]);
gauge_vec!(  BENCH_TPS_CURRENT,   "contra_bench_tps_current_tps",   "Instantaneous TPS over last 5s",       &[]);
// RTT histogram: use prometheus::register_histogram_vec! directly
// (histogram_vec! macro doesn't support custom buckets)
// Buckets in seconds: [0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5]
```

If `--metrics-port` given: `start_metrics_server(port)` → Prometheus scrapes → Grafana dashboards. If absent: stdout-only via tracing.

---

## CLI Usage Examples

```bash
# Build
cargo build --release -p contra-core --bin contra-bench-tps

# No conflicts (pure throughput test — stresses sigverify/executor)
./target/release/contra-bench-tps \
  --rpc-url http://localhost:8899 \
  --admin-keypair ./admin-keypair.json \
  --accounts 50 --threads 20 --duration 60

# Max conflicts (stresses sequencer — all txs fight over 1 destination)
./target/release/contra-bench-tps \
  --rpc-url http://localhost:8899 \
  --admin-keypair ./admin-keypair.json \
  --accounts 50 --threads 20 --duration 60 \
  --num-conflict-groups 1

# With Prometheus metrics on port 9101
./target/release/contra-bench-tps \
  --rpc-url http://localhost:8899 \
  --admin-keypair ./admin-keypair.json \
  --accounts 50 --threads 50 --duration 120 \
  --metrics-port 9101
```

---

## Verification

1. `cargo build -p contra-core --bin contra-bench-tps` compiles without errors
2. Start contra stack: `docker compose up -d` (local validator + node + postgres)
3. Run setup-only sanity: `--accounts 5 --threads 5 --duration 10` → stdout shows TPS > 0, success_rate > 0
4. Run no-conflict mode → observe `send_success` rate in logs
5. Run `--num-conflict-groups 1` → observe lower TPS vs no-conflict mode (sequencer pressure visible)
6. With `--metrics-port 9101`, confirm `curl http://localhost:9101/metrics` returns Prometheus text format
7. Graph `rate(contra_bench_tps_sent_total[1m])` in Grafana → should track stdout TPS output

---

---

# Part 2: Node-Side Bottleneck Detection

## Context

The bench-tps binary produces load but cannot tell you *where* the pipeline slows down. Without per-stage instrumentation in the node, all you can observe is the external TPS ceiling. This section adds a `StageMetrics` trait to the pipeline so each stage reports its own throughput and latency — making the bottleneck directly visible in Grafana.

The design is opt-in via a `--metrics-port` flag on `contra-node`. When absent, stages use a zero-cost `NoopMetrics` implementation that emits only debug logs. No production behavior changes.

---

## Files to Create / Modify

| File | Change |
|------|--------|
| `core/src/stage_metrics.rs` | **New file** — trait + both implementations |
| `core/src/lib.rs` | Add `pub mod stage_metrics;` |
| `core/src/nodes/node.rs` | Add `metrics` field to `NodeConfig`; wire into each stage's args |
| `core/src/bin/node.rs` | Add `--metrics-port` CLI flag; construct correct impl |
| `core/src/stages/dedup.rs` | Add `metrics` to `DedupArgs`; instrument hot path |
| `core/src/stages/sigverify.rs` | Add `metrics` to `SigverifyArgs`; instrument verify loop |
| `core/src/stages/sequencer.rs` | Add `metrics` to `SequencerArgs`; instrument batch loop |
| `core/src/stages/execution.rs` | Add `metrics` to executor args; instrument execution + account load |
| `core/src/stages/settle.rs` | Add `metrics` to settler args; instrument DB write |
| `core/Cargo.toml` | Add `contra-metrics = { path = "../metrics" }` (shared with bench-tps) |

---

## New File: `core/src/stage_metrics.rs`

### The Trait

```rust
use std::sync::Arc;

pub trait StageMetrics: Send + Sync {
    // Dedup
    fn dedup_received(&self);
    fn dedup_forwarded(&self);
    fn dedup_dropped_duplicate(&self);
    fn dedup_dropped_unknown_blockhash(&self);

    // Sigverify
    fn sigverify_forwarded(&self);
    fn sigverify_rejected(&self, reason: &'static str);
    fn sigverify_duration_ms(&self, ms: f64);

    // Sequencer
    fn sequencer_collected(&self, tx_count: usize);
    fn sequencer_batches_emitted(&self, batch_count: usize);
    fn sequencer_conflict_ratio(&self, ratio: f64); // num_batches / num_txs

    // Executor
    fn executor_execution_ms(&self, ms: f64);
    fn executor_accounts_preloaded(&self, count: usize);

    // Settler
    fn settler_txs_settled(&self, count: usize);
    fn settler_db_write_ms(&self, ms: f64);
}

pub type SharedMetrics = Arc<dyn StageMetrics>;
```

### `NoopMetrics` (default — debug logs only)

```rust
pub struct NoopMetrics;

impl StageMetrics for NoopMetrics {
    fn dedup_received(&self)                          { tracing::debug!("dedup: received"); }
    fn dedup_forwarded(&self)                         { tracing::debug!("dedup: forwarded"); }
    fn dedup_dropped_duplicate(&self)                 { tracing::debug!("dedup: dropped duplicate"); }
    fn dedup_dropped_unknown_blockhash(&self)         { tracing::debug!("dedup: dropped unknown blockhash"); }
    fn sigverify_forwarded(&self)                     { tracing::debug!("sigverify: forwarded"); }
    fn sigverify_rejected(&self, reason: &'static str){ tracing::debug!("sigverify: rejected reason={}", reason); }
    fn sigverify_duration_ms(&self, ms: f64)          { tracing::debug!("sigverify: {:.3}ms", ms); }
    fn sequencer_collected(&self, n: usize)           { tracing::debug!("sequencer: collected {}", n); }
    fn sequencer_batches_emitted(&self, n: usize)     { tracing::debug!("sequencer: emitted {} batches", n); }
    fn sequencer_conflict_ratio(&self, r: f64)        { tracing::debug!("sequencer: conflict_ratio={:.2}", r); }
    fn executor_execution_ms(&self, ms: f64)          { tracing::debug!("executor: {:.3}ms", ms); }
    fn executor_accounts_preloaded(&self, n: usize)   { tracing::debug!("executor: preloaded {} accounts", n); }
    fn settler_txs_settled(&self, n: usize)           { tracing::debug!("settler: settled {}", n); }
    fn settler_db_write_ms(&self, ms: f64)            { tracing::debug!("settler: db write {:.3}ms", ms); }
}
```

### `PrometheusMetrics` (enabled via `--metrics-port`)

```rust
use contra_metrics::{counter_vec, gauge_vec, histogram_vec, init_metrics};
use prometheus::register_histogram_vec;

// Counters
counter_vec!(DEDUP_RECEIVED,          "contra_dedup_received_total",             "...", &[]);
counter_vec!(DEDUP_FORWARDED,         "contra_dedup_forwarded_total",            "...", &[]);
counter_vec!(DEDUP_DROPPED_DUP,       "contra_dedup_dropped_duplicate_total",    "...", &[]);
counter_vec!(DEDUP_DROPPED_UNK_BH,    "contra_dedup_dropped_unknown_bh_total",   "...", &[]);
counter_vec!(SIGVERIFY_FORWARDED,     "contra_sigverify_forwarded_total",        "...", &[]);
counter_vec!(SIGVERIFY_REJECTED,      "contra_sigverify_rejected_total",         "...", &["reason"]);
counter_vec!(SETTLER_TXS_SETTLED,     "contra_settler_txs_settled_total",        "...", &[]);

// Histograms (registered directly for custom buckets in seconds)
// sigverify: [0.0001, 0.0005, 0.001, 0.005, 0.01, 0.05, 0.1]
// executor:  [0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0]
// settler:   [0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 5.0]

// Gauges
gauge_vec!(SEQUENCER_CONFLICT_RATIO,  "contra_sequencer_conflict_ratio",         "...", &[]);
gauge_vec!(EXECUTOR_ACCOUNTS_LOADED,  "contra_executor_accounts_preloaded",      "...", &[]);
```

`PrometheusMetrics` implements `StageMetrics` by calling `.inc()`, `.observe()`, `.set()` on the statics above. It holds no state of its own — all state is in the global Prometheus registry.

```rust
pub struct PrometheusMetrics;

impl StageMetrics for PrometheusMetrics {
    fn dedup_received(&self)      { DEDUP_RECEIVED.with_label_values(&[]).inc(); }
    fn dedup_forwarded(&self)     { DEDUP_FORWARDED.with_label_values(&[]).inc(); }
    fn dedup_dropped_duplicate(&self) { DEDUP_DROPPED_DUP.with_label_values(&[]).inc(); }
    // ... etc for all methods
    fn sigverify_rejected(&self, reason: &'static str) {
        SIGVERIFY_REJECTED.with_label_values(&[reason]).inc();
    }
    fn sigverify_duration_ms(&self, ms: f64) {
        SIGVERIFY_DURATION.with_label_values(&[]).observe(ms / 1000.0);
    }
    // ... etc
}

pub fn init_prometheus_metrics() {
    init_metrics!(
        DEDUP_RECEIVED, DEDUP_FORWARDED, DEDUP_DROPPED_DUP, DEDUP_DROPPED_UNK_BH,
        SIGVERIFY_FORWARDED, SIGVERIFY_REJECTED, SETTLER_TXS_SETTLED,
        SEQUENCER_CONFLICT_RATIO, EXECUTOR_ACCOUNTS_LOADED
    );
    // Force-init histogram statics too
}
```

---

## `core/src/bin/node.rs` Changes

Add one field to `Args`:

```rust
/// Enable Prometheus stage metrics server (load testing / profiling only).
/// When absent, stages emit debug logs only.
#[arg(long, env = "CONTRA_METRICS_PORT")]
metrics_port: Option<u16>,
```

In `run_node_with_args`, construct the right implementation before building `NodeConfig`:

```rust
let metrics: Arc<dyn StageMetrics> = match args.metrics_port {
    Some(port) => {
        init_prometheus_metrics();
        start_metrics_server(port);
        info!("Stage metrics enabled on port {}", port);
        Arc::new(PrometheusMetrics)
    }
    None => Arc::new(NoopMetrics),
};

let config = NodeConfig {
    // ... existing fields ...
    metrics,
};
```

---

## `core/src/nodes/node.rs` Changes

Add one field to `NodeConfig`:

```rust
pub struct NodeConfig {
    // ... existing fields unchanged ...
    pub metrics: SharedMetrics,
}
```

Update `Default` impl:

```rust
metrics: Arc::new(NoopMetrics),
```

In `run_node()`, clone into each stage's args:

```rust
start_dedup(DedupArgs {
    // ... existing ...
    metrics: Arc::clone(&config.metrics),
}).await;

start_sigverify_workerpool(SigverifyArgs {
    // ... existing ...
    metrics: Arc::clone(&config.metrics),
}).await;

start_sequence_worker(SequencerArgs {
    // ... existing ...
    metrics: Arc::clone(&config.metrics),
}).await;

// same for executor and settler
```

---

## Per-Stage Instrumentation

### Dedup (`core/src/stages/dedup.rs`)

Add `metrics: SharedMetrics` to `DedupArgs`. Instrument the transaction arm of the select loop:

```rust
Some(transaction) => {
    args.metrics.dedup_received();                        // line 158 — new

    if !live_blockhashes_clone.read()...contains(&blockhash) {
        args.metrics.dedup_dropped_unknown_blockhash();   // line 165 — replaces TODO
        warn!(...);
        continue;
    }

    if is_duplicate {
        args.metrics.dedup_dropped_duplicate();           // line 176 — replaces TODO comment
        warn!(...);
        continue;
    }

    args.metrics.dedup_forwarded();                       // line 189 — new
    output_tx.send(transaction).await...
}
```

**What this reveals**: `dropped_duplicate` rate high → load test is resending same tx. `dropped_unknown_blockhash` rate high → blockhash poller in bench-tps is too slow.

---

### Sigverify (`core/src/stages/sigverify.rs`)

Add `metrics: SharedMetrics` to `SigverifyArgs`. Instrument each worker's inner loop:

```rust
Ok(Some(transaction)) => {
    let t0 = std::time::Instant::now();
    let result = sigverify_transaction(&transaction, &admin_keys).await;
    let ms = t0.elapsed().as_secs_f64() * 1000.0;

    match result {
        SigverifyResult::Valid(_) => {
            args.metrics.sigverify_forwarded();
            // existing send to sequencer...
        }
        SigverifyResult::InvalidTransaction(_) => {
            args.metrics.sigverify_rejected("invalid");
            // existing warn...
        }
        SigverifyResult::NotSignedByAdmin => {
            args.metrics.sigverify_rejected("not_admin");
        }
        SigverifyResult::SigverifyFailed(_) => {
            args.metrics.sigverify_rejected("sig_failed");
        }
    }
    args.metrics.sigverify_duration_ms(ms);   // always record, regardless of outcome
}
```

**What this reveals**: If `sigverify_duration_ms` p95 is high, add more `--sigverify-workers`. If throughput plateaus below load, the bounded queue (size 1000) is backing up.

---

### Sequencer (`core/src/stages/sequencer.rs`)

Add `metrics: SharedMetrics` to `SequencerArgs`. Instrument `process_and_send_batches` (already takes `&mut scheduler`, `transactions`, `batch_tx`):

```rust
// After collecting transactions (line ~97), before process_and_send_batches:
args.metrics.sequencer_collected(collected);

// Inside process_and_send_batches, after scheduling:
let num_batches = conflict_free_batches.len();
let num_txs = transactions.len();
if num_txs > 0 {
    args.metrics.sequencer_batches_emitted(num_batches);
    args.metrics.sequencer_conflict_ratio(num_batches as f64 / num_txs as f64);
}
```

**What this reveals**: `conflict_ratio` = 1.0 means every tx conflicts with every other → sequencer emits 1 batch per tx → floods executor. Run with `--num-conflict-groups 1` in bench-tps to reproduce this intentionally. Compare against `--num-conflict-groups N` (no conflicts) to quantify sequencer overhead.

---

### Executor (`core/src/stages/execution.rs`)

Add `metrics: SharedMetrics` to executor args. Instrument two points:

```rust
// Before account preload (before the fetch_accounts call):
let t_preload = std::time::Instant::now();
// ... existing account preload logic ...
let accounts_loaded = /* count of accounts fetched */;
args.metrics.executor_accounts_preloaded(accounts_loaded);

// Before SVM execution:
let t_exec = std::time::Instant::now();
// ... existing execute_transactions call ...
args.metrics.executor_execution_ms(t_exec.elapsed().as_secs_f64() * 1000.0);
```

**What this reveals**: If `executor_execution_ms` p95 is high with low `accounts_preloaded`, SVM execution is the bottleneck. If `accounts_preloaded` is high and execution is slow, Postgres/Redis account loading is the bottleneck.

---

### Settler (`core/src/stages/settle.rs`)

Add `metrics: SharedMetrics` to settler args. Instrument the DB write:

```rust
// Before the batch INSERT:
let t_db = std::time::Instant::now();
// ... existing DB write ...
let txs_settled = /* count of transactions settled */;
args.metrics.settler_db_write_ms(t_db.elapsed().as_secs_f64() * 1000.0);
args.metrics.settler_txs_settled(txs_settled);
```

**What this reveals**: If `settler_db_write_ms` is consistently close to `blocktime_ms` (100ms), the settler is at capacity. Increasing batch size (`--max-tx-per-batch`) makes this worse.

---

## Bottleneck Identification Guide

With all metrics in place, run bench-tps with ramping `--threads` and watch Grafana:

| Observation | Bottleneck | Action |
|-------------|-----------|--------|
| `contra_sigverify_forwarded_total` rate plateaus, `sigverify_duration_ms` p95 high | Sigverify CPU | Increase `--sigverify-workers` |
| `contra_dedup_forwarded_total` rate grows but `sigverify_forwarded` lags | Sigverify queue full (size 1000) | Increase `CONTRA_SIGVERIFY_QUEUE_SIZE` |
| `conflict_ratio` = 1.0, sequencer throughput drops | Sequencer DAG scheduler | Reduce conflict groups or tune `--max-tx-per-batch` |
| `executor_execution_ms` high, `accounts_preloaded` low | SVM execution | Profile SVM call; consider smaller batches |
| `executor_execution_ms` high, `accounts_preloaded` high | DB/Redis account load | Tune Redis cache or add read replicas |
| `settler_db_write_ms` > 80ms | Postgres write bottleneck | Tune batch size, add write pooling |

---

## Node CLI Usage (Load Testing Mode)

```bash
# Start node with stage metrics enabled on port 9090
contra-node \
  --port 8899 \
  --accountsdb-connection-url postgresql://... \
  --metrics-port 9090

# Then run bench-tps against it
contra-bench-tps \
  --rpc-url http://localhost:8899 \
  --admin-keypair ./admin-keypair.json \
  --accounts 50 --threads 20 --duration 60 \
  --metrics-port 9101

# Prometheus scrapes both :9090 (node stages) and :9101 (bench-tps client)
# Grafana shows per-stage throughput + client TPS on one dashboard
```

---

## Full Verification (Node + Bench-TPS Together)

1. Build both binaries: `cargo build --release -p contra-core`
2. Start contra stack with `--metrics-port 9090`
3. Run bench-tps with `--metrics-port 9101`
4. Confirm `/metrics` on both ports returns stage counters
5. In Grafana, confirm all stage counters increment during the test
6. Run with `--num-conflict-groups 1`: confirm `contra_sequencer_conflict_ratio` → 1.0
7. Run with `--num-conflict-groups 50` (default): confirm ratio drops to ~0.02
8. The stage where `rate(counter[1m])` stops growing first as `--threads` increases = the bottleneck
