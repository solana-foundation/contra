# Proposed Simplify Changes

Generated from `/simplify` review. Apply manually when ready.

---

## 1. `core/src/stage_metrics.rs` — Pre-resolve metric handles in PrometheusMetrics

**Why**: Every `with_label_values(&[])` call acquires a `parking_lot::RwLock`, computes an FNV hash,
and clones an `Arc`. At high TPS this happens tens of thousands of times per second across all stages.
Pre-resolving at construction time makes each hot-path call a direct atomic op.

Replace `pub struct PrometheusMetrics;` and its `impl StageMetrics` block with:

```rust
/// Pre-resolves all metric handles once so hot-path calls are lock-free atomic ops.
pub struct PrometheusMetrics {
    dedup_received: prometheus::Counter,
    dedup_forwarded: prometheus::Counter,
    dedup_dropped_dup: prometheus::Counter,
    dedup_dropped_unk_bh: prometheus::Counter,
    sigverify_forwarded: prometheus::Counter,
    sigverify_rejected: prometheus::CounterVec,
    sequencer_collected: prometheus::Counter,
    sequencer_txs_emitted: prometheus::Counter,
    settler_txs_settled: prometheus::Counter,
}

impl PrometheusMetrics {
    pub fn new() -> Self {
        let nl: &[&str] = &[];
        Self {
            dedup_received:           DEDUP_RECEIVED.with_label_values(nl),
            dedup_forwarded:          DEDUP_FORWARDED.with_label_values(nl),
            dedup_dropped_dup:        DEDUP_DROPPED_DUP.with_label_values(nl),
            dedup_dropped_unk_bh:     DEDUP_DROPPED_UNK_BH.with_label_values(nl),
            sigverify_forwarded:      SIGVERIFY_FORWARDED.with_label_values(nl),
            sigverify_rejected:       SIGVERIFY_REJECTED.clone(),
            sequencer_collected:      SEQUENCER_COLLECTED.with_label_values(nl),
            sequencer_txs_emitted:    SEQUENCER_TXS_EMITTED.with_label_values(nl),
            settler_txs_settled:      SETTLER_TXS_SETTLED.with_label_values(nl),
        }
    }
}

impl StageMetrics for PrometheusMetrics {
    fn dedup_received(&self)                      { self.dedup_received.inc(); }
    fn dedup_forwarded(&self)                     { self.dedup_forwarded.inc(); }
    fn dedup_dropped_duplicate(&self)             { self.dedup_dropped_dup.inc(); }
    fn dedup_dropped_unknown_blockhash(&self)     { self.dedup_dropped_unk_bh.inc(); }
    fn sigverify_forwarded(&self)                 { self.sigverify_forwarded.inc(); }
    fn sigverify_rejected(&self, reason: &'static str) {
        self.sigverify_rejected.with_label_values(&[reason]).inc();
    }
    fn sequencer_collected(&self, n: usize)        { self.sequencer_collected.inc_by(n as f64); }
    fn sequencer_transactions_emitted(&self, n: usize)  { self.sequencer_txs_emitted.inc_by(n as f64); }
    fn settler_txs_settled(&self, n: usize)        { self.settler_txs_settled.inc_by(n as f64); }
}
```

Also update `core/src/bin/node.rs` line:
```rust
// Before:
Arc::new(PrometheusMetrics)
// After:
Arc::new(PrometheusMetrics::new())
```

---

## 2. `core/src/stages/sigverify.rs` — Remove unnecessary comment (line ~155)

Delete:
```rust
// metrics is already an Arc; clone it for each worker
```

The `SharedMetrics` type alias makes this self-evident.

---

## Notes (won't fix)

- `LazyLock` + `register_histogram_vec!` for custom-bucket histograms: cannot use `histogram_vec!`
  macro because it doesn't accept a bucket list. Pattern is correct as-is.
