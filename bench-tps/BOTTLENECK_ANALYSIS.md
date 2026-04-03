# Bottleneck Analysis Guide

How to use the `Contra Bench` Grafana dashboard to identify bottlenecks across
all three bench flows.  The dashboard is structured to mirror the pipeline order
— data flows left-to-right / top-to-bottom in each section.

---

## Transfer flow (Contra pipeline)

### Dashboard: Bench TPS + Pipeline Stages

The transfer flow exercises the full Contra processing pipeline:

```
Sent TPS → Dedup → Sigverify → Sequencer → Executor → Settler → Landed TPS
```

**Healthy steady state:** all rates approximately equal:
```
Sent ≈ Dedup forwarded ≈ Sigverify forwarded ≈ Sequencer emitted
      ≈ Executor results ≈ Settler settled ≈ Landed TPS
```

Any persistent gap at a stage is the bottleneck.

### Landed TPS — how it is calculated

`rate(contra_bench_tps_landed_total[10s])` — incremented by `getTransactionCount`
delta every second.  `getTransactionCount` reads from the Postgres metadata table
updated by the **settler** on the primary, then replicated to the read node.

Sources of lag:

| Source | Effect |
|--------|--------|
| Pipeline depth | Ramp-up/ramp-down lag; at steady state the rate is accurate |
| Replication lag | Bench polls the **read node** (replica). If Settler > Landed TPS, replication is the lag source |

Cross-check: `rate(contra_settler_txs_settled_total[10s])` in the Settler panel
is the ground truth (no replication lag).

### Panel-by-panel signals

**Dedup Throughput**

| Series | Metric | Signal when elevated |
|--------|--------|----------------------|
| Received | `contra_dedup_received_total` | Baseline input rate |
| Forwarded | `contra_dedup_forwarded_total` | — |
| Dropped (dup) | `contra_dedup_dropped_duplicate_total` | Bench reusing tx signatures — check memo nonce |
| Dropped (bh) | `contra_dedup_dropped_unknown_bh_total` | Blockhash poller lagging; blockhash too old |

**Sigverify Throughput**

| Series | Metric | Signal |
|--------|--------|--------|
| Forwarded | `contra_sigverify_forwarded_total` | Lower than dedup → increase `CONTRA_SIGVERIFY_WORKERS` |
| Rejected (sig_failed) | `contra_sigverify_rejected_total` | Signing error in bench |
| Rejected (invalid/not_admin) | `contra_sigverify_rejected_total` | Wrong program or key |

**Sequencer Throughput**

- `Collected` < `Sigverify forwarded` → executor is the bottleneck; backpressure visible here
- `Emitted` should equal `Collected` — sequencer reorders into conflict-free batches, never drops

**Executor Throughput**

- `Results sent` < `Sequencer emitted` → SVM execution is the bottleneck (most CPU-intensive stage)
- `Send failed` or `Missing results` non-zero → executor error, check node logs

**Settler Throughput**

- `Settled` < `Executor results` → DB write throughput is the bottleneck; check Postgres I/O
- `Settled` ≈ `Executor results` but `Landed TPS` lower → Postgres replication lag

### Bottleneck decision tree

```
Sent >> Landed at steady state?
│
├─ Dedup Dropped (bh) high?   → blockhash poller lagging; reduce send rate
├─ Dedup Dropped (dup) high?  → duplicate signatures; memo nonce not incrementing
├─ Sigverify Forwarded << Dedup Forwarded?  → increase CONTRA_SIGVERIFY_WORKERS
├─ Sequencer Collected << Sigverify Forwarded?  → executor saturated (backpressure)
├─ Executor Results << Sequencer Emitted?  → SVM is the bottleneck; check CPU
├─ Settler Settled << Executor Results?  → Postgres write throughput; check I/O
└─ Settler ≈ Executor but Landed lower?  → replication lag; Settler is ground truth
```

### Key config knobs

| Symptom | Knob | File |
|---------|------|------|
| Sigverify bottleneck | `CONTRA_SIGVERIFY_WORKERS` | `.env` |
| Sigverify queue full | `CONTRA_SIGVERIFY_QUEUE_SIZE` | `.env` |
| Batch size vs conflict ratio | `CONTRA_MAX_TX_PER_BATCH` | `.env` |
| DB connection exhaustion | `CONTRA_WRITE_MAX_CONNECTIONS` | `.env` |

---

## Deposit flow (Solana → Contra)

### Pipeline

```
bench (Solana Deposit tx)
  → Solana validator confirms
    → indexer-solana detects event, saves to DB
      → operator-solana fetches from DB, sends Contra mint
```

### Dashboard: Deposit Flow (Solana → Contra)

Four panels in pipeline order:

| Panel | Metric | What to look for |
|-------|--------|-----------------|
| **1. Solana Sent TPS** | `rate(contra_bench_tps_sent_total{flow="deposit"}[10s])` | Bench throughput to Solana |
| **2. Indexer — Solana Events Indexed** | `rate(contra_indexer_transactions_saved_total{program_type="escrow"}[10s])` + `rate(contra_indexer_mints_saved_total{program_type="escrow"}[10s])` | Indexer pickup rate; `mints_saved` feeds operator queue |
| **3. Operator — Processing Pipeline** | `rate(contra_operator_transactions_fetched_total{program_type="escrow"}[10s])` + `contra_operator_backlog_depth{program_type="escrow"}` | Operator poll rate; rising backlog = operator can't keep up |
| **4. Contra Mint Rate** | `rate(contra_operator_mints_sent_total{program_type="escrow"}[10s])` | End-to-end confirmed Contra mints |

### Signals

- **Panel 1 rate is low** — sender threads blocked on Solana RPC latency; increase `BENCH_THREADS`
- **Gap between panel 1 and panel 2** — Solana validator not confirming (fee exhaustion, escrow account contention); check validator logs
- **Panel 2 `transactions_saved` grows but `mints_saved` doesn't** — indexer indexed the slot but failed to classify the deposit event; check indexer-solana logs
- **Panel 3 backlog grows** — operator is fetching but can't send Contra mints fast enough; check operator-solana logs for RPC errors
- **Panel 4 is zero** — operator-solana is not running or `COMMON_ESCROW_INSTANCE_ID` does not match the bench's instance PDA

### Throughput ceiling

The escrow instance ATA is a single shared writable account — the Solana validator
serialises all writes to it.  This is the hard ceiling for deposit TPS and
cannot be raised by adding more depositor accounts.  Typical ceiling on a local
validator: 500–2000 TPS depending on hardware.

### Config knobs

| Symptom | Knob |
|---------|------|
| Low Solana send rate | Increase `BENCH_THREADS` |
| No e2e measurement | Set `BENCH_OPERATOR_METRICS_URL=http://localhost:9102/metrics` |
| Instance PDA mismatch | Ensure `COMMON_ESCROW_INSTANCE_ID` in `.env` matches the bench's seed keypair |

---

## Withdraw flow (Contra → Solana)

### Pipeline

```
bench (Contra WithdrawFunds / burn)
  → Contra write-node confirms (dedup → sigverify → sequencer → executor → settler)
    → indexer-contra detects burn event, saves to DB
      → operator-contra fetches from DB, sends Solana ReleaseFunds
```

### Dashboard: Withdraw Flow (Contra → Solana)

Four panels in pipeline order:

| Panel | Metric | What to look for |
|-------|--------|-----------------|
| **1. Contra Sent / Landed TPS** | `rate(contra_bench_tps_sent_total{flow="withdraw"}[10s])` + `rate(contra_bench_tps_landed_total{flow="withdraw"}[10s])` | Bench send rate and Contra confirmation rate |
| **2. Indexer — Contra Events Indexed** | `rate(contra_indexer_transactions_saved_total{program_type="withdraw"}[10s])` + `rate(contra_indexer_mints_saved_total{program_type="withdraw"}[10s])` | Indexer pickup rate; `mints_saved` feeds operator queue |
| **3. Operator — Processing Pipeline** | `rate(contra_operator_transactions_fetched_total{program_type="withdraw"}[10s])` + `contra_operator_backlog_depth{program_type="withdraw"}` | Operator poll rate; rising backlog = operator can't keep up |
| **4. Solana Release Rate** | `rate(contra_operator_mints_sent_total{program_type="withdraw"}[10s])` | End-to-end confirmed Solana releases |

### Signals

- **Gap between Sent and Landed (panel 1)** — Contra pipeline is dropping transactions; switch to the Pipeline Stages section to identify which Contra stage is the bottleneck (same analysis as transfer flow above)
- **Panel 2 `transactions_saved` grows but `mints_saved` doesn't** — indexer-contra indexed the slot but failed to classify the burn event; check indexer-contra logs
- **Panel 3 backlog grows** — operator-contra is fetching but Solana RPC latency is high or ReleaseFunds transactions are failing; check operator-contra logs
- **Panel 4 is zero** — operator-contra not running, `COMMON_SOURCE_RPC_URL` not pointing to the Contra gateway, or the instance PDA does not match
- **`invalid instruction data` errors in operator logs** — withdrawer Solana ATAs were not created during setup; this should be handled by `setup_withdraw.rs` automatically

### Balance exhaustion

Each withdraw burns 1 raw token unit from the withdrawer's Contra ATA.  If the
load phase runs longer than `initial_balance / tps` seconds, accounts drain
to zero and subsequent transactions fail silently.  Default
`--initial-balance 1_000_000` supports ~20 000 s at 50 TPS.

### Config knobs

| Symptom | Knob |
|---------|------|
| Low Contra send rate | Increase `BENCH_THREADS` |
| Contra pipeline bottleneck | See Transfer flow decision tree above |
| No e2e measurement | Set `BENCH_WITHDRAW_OPERATOR_METRICS_URL=http://localhost:9103/metrics` |
| Instance PDA mismatch | `BENCH_INSTANCE_SEED_KEYPAIR` must match `COMMON_ESCROW_INSTANCE_ID` |
| Balance exhaustion | Increase `BENCH_INITIAL_BALANCE` or reduce `BENCH_DURATION` |
