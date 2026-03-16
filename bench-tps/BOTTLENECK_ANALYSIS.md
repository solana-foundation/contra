# Bottleneck Analysis Guide

How to use the Grafana `Contra Bench` dashboard to identify pipeline bottlenecks
and verify the Landed TPS metric.

---

## Landed TPS — How It Is Calculated

The **Landed TPS** panel shows `contra_bench_tps_current_tps`, a Gauge updated by
the bench binary every second:

```
tps = getTransactionCount() − prev_count
BENCH_TPS_CURRENT.set(tps)
```

`getTransactionCount` reads the `transaction_count` key from the Postgres metadata
table. That counter is incremented by the **settler** each time it commits a batch
to the DB. So:

> **Landed TPS = transactions that completed the full pipeline and were written to
> the database, as reported by the read node.**

### Sources of Lag

| Source | Effect |
|--------|--------|
| **Pipeline depth** | A transaction sent now won't be settled for several ms. The panel lags at ramp-up and ramp-down; at steady state the rate is correct. |
| **Replication lag** | The bench polls `getTransactionCount` via the **read node** (Postgres replica). The settler writes to the **primary**. Replication lag causes the panel to understate reality. |

**Ground-truth cross-check**: compare Landed TPS against
`rate(contra_settler_txs_settled_total[10s])` in the **Settler Throughput** panel.
The settler counter is on the write node (no replication lag). If Settler > Landed
TPS, replication is the lag source, not the pipeline.

---

## Pipeline Waterfall

Every transaction passes through five stages in order. Each stage's output feeds
the next. The bottleneck is the first stage whose output rate drops and stays
below the previous stage.

```
Sent TPS  →  Dedup  →  Sigverify  →  Sequencer  →  Executor  →  Settler  →  Landed TPS
```

At healthy steady state all rates should be approximately equal:

```
Sent TPS ≈ Dedup forwarded ≈ Sigverify forwarded ≈ Sequencer emitted
         ≈ Executor results sent ≈ Settler settled ≈ Landed TPS
```

Any persistent gap at a specific stage is the bottleneck.

---

## Panel-by-Panel Reading Guide

### Bench TPS row

**Sent TPS (Sender Threads)**
`irate(contra_bench_tps_sent_total[5s])`

The raw send rate from the bench. This is the demand being placed on the node.

**Landed TPS**
`contra_bench_tps_current_tps`

Transactions confirmed as settled. Compare to Sent TPS — the gap is total pipeline
loss. If Sent ≫ Landed at steady state, something in the stages below is dropping.

---

### Pipeline Stages row

**Dedup Throughput**

| Series | Metric | Meaning |
|--------|--------|---------|
| Received | `contra_dedup_received_total` | Everything arriving at the node |
| Forwarded | `contra_dedup_forwarded_total` | Passed dedup, sent to sigverify |
| Dropped (dup) | `contra_dedup_dropped_duplicate_total` | Seen before — signature reused |
| Dropped (bh) | `contra_dedup_dropped_unknown_bh_total` | Blockhash not in the live window |

Signals:
- `Dropped (bh)` spike → blockhash poller is lagging; transactions arrive with a
  hash the node hasn't accepted yet.
- `Dropped (dup)` spike → the bench is recycling transactions; check memo nonce
  generation.
- Large `Received − Forwarded` gap with no drops → dedup itself is the bottleneck
  (rare; dedup is single-threaded but very fast).

---

**Sigverify Throughput**

| Series | Metric | Meaning |
|--------|--------|---------|
| Forwarded | `contra_sigverify_forwarded_total` | Valid, passed to sequencer |
| Rejected (reason) | `contra_sigverify_rejected_total{reason}` | Broken down by `invalid` / `not_admin` / `sig_failed` |

Signals:
- `Forwarded` rate is lower than `Dedup forwarded` → sigverify workers are
  saturated. Increase `CONTRA_SIGVERIFY_WORKERS`.
- `Rejected (sig_failed)` spike → signing error in the bench.
- `Rejected (invalid)` → empty or mixed transactions being sent.

---

**Sequencer Throughput**

| Series | Metric | Meaning |
|--------|--------|---------|
| Collected/s | `contra_sequencer_collected_total` | Transactions drained from sigverify |
| Emitted/s | `contra_sequencer_transactions_emitted_total` | Transactions sent to executor after scheduling |

Signals:
- `Collected` rate is lower than `Sigverify forwarded` → the sequencer → executor
  channel is full (executor is the bottleneck, backpressure reaches here).
- `Emitted` should equal `Collected` — the sequencer emits every transaction it
  collects, just reordered into conflict-free batches.

---

**Executor Throughput**

| Series | Metric | Meaning |
|--------|--------|---------|
| Results sent/s | `contra_executor_results_sent_total` | Batches handed off to settler |
| Send failed/s (kind) | `contra_executor_results_send_failed_total{kind}` | Settler channel closed mid-run |
| Missing results/s (kind) | `contra_executor_missing_results_total{kind}` | SVM returned no output for a batch |

Signals:
- `Results sent` lower than `Sequencer emitted` → SVM execution is the bottleneck.
  This is the most CPU-intensive stage.
- `Send failed` or `Missing results` non-zero → executor error, check node logs.

---

**Settler Throughput**

| Series | Metric | Meaning |
|--------|--------|---------|
| Settled/s | `contra_settler_txs_settled_total` | Transactions written to Postgres |

Signals:
- `Settled` lower than `Executor results sent` → DB write is the bottleneck.
  Check Postgres CPU, connection pool (`CONTRA_WRITE_MAX_CONNECTIONS`), and disk I/O.
- `Settled` matches `Executor results sent` but `Landed TPS` is lower →
  Postgres replication lag (see above).

---

## Bottleneck Decision Tree

```
Sent TPS >> Landed TPS at steady state?
│
├─ Dedup: Dropped (bh) high?
│   └─ Yes → Blockhash poller can't keep up; reduce send rate or check RPC latency
│
├─ Dedup: Dropped (dup) high?
│   └─ Yes → Bench is reusing transaction signatures
│
├─ Sigverify Forwarded << Dedup Forwarded?
│   └─ Yes → Increase CONTRA_SIGVERIFY_WORKERS
│
├─ Sequencer Collected << Sigverify Forwarded?
│   └─ Yes → Executor is saturated; backpressure visible at sequencer
│
├─ Executor Results Sent << Sequencer Emitted?
│   └─ Yes → SVM execution is the bottleneck; check CPU utilisation
│
├─ Settler Settled << Executor Results Sent?
│   └─ Yes → Postgres write throughput is the bottleneck
│
└─ Settler Settled ≈ Executor Results Sent, but Landed TPS lower?
    └─ Postgres replication lag; Settler panel is ground truth
```

---

## Key Config Knobs

| Symptom | Knob | Location |
|---------|------|----------|
| Sigverify bottleneck | `CONTRA_SIGVERIFY_WORKERS` | `.env` |
| Sigverify queue full | `CONTRA_SIGVERIFY_QUEUE_SIZE` | `.env` |
| Batch size vs conflict ratio | `CONTRA_MAX_TX_PER_BATCH` | `.env` |
| DB connection exhaustion | `CONTRA_WRITE_MAX_CONNECTIONS` | `.env` |
| Blocktime affects settler cadence | `CONTRA_BLOCKTIME_MS` | `.env` |
