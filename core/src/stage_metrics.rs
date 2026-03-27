use std::sync::Arc;
use tracing::debug;

/// Instrumentation trait — each stage calls into this; no pipeline logic changes.
pub trait StageMetrics: Send + Sync {
    // Dedup
    fn dedup_received(&self);
    fn dedup_forwarded(&self);
    fn dedup_dropped_duplicate(&self);
    fn dedup_dropped_unknown_blockhash(&self);

    // Sigverify
    fn sigverify_forwarded(&self);
    fn sigverify_rejected(&self, reason: &'static str);

    // Sequencer
    fn sequencer_collected(&self, tx_count: usize);
    fn sequencer_transactions_emitted(&self, tx_count: usize);

    // Executor
    fn executor_results_sent(&self, tx_count: usize);
    fn executor_results_send_failed(&self, kind: &'static str);
    fn executor_missing_results(&self, kind: &'static str);

    // Settler
    fn settler_txs_settled(&self, count: usize);
}

pub type SharedMetrics = Arc<dyn StageMetrics>;

// ---------------------------------------------------------------------------
// NoopMetrics — zero overhead in production; emits debug logs only.
// ---------------------------------------------------------------------------

pub struct NoopMetrics;

impl StageMetrics for NoopMetrics {
    fn dedup_received(&self) {
        debug!("dedup: received");
    }
    fn dedup_forwarded(&self) {
        debug!("dedup: forwarded");
    }
    fn dedup_dropped_duplicate(&self) {
        debug!("dedup: dropped duplicate");
    }
    fn dedup_dropped_unknown_blockhash(&self) {
        debug!("dedup: dropped unknown blockhash");
    }
    fn sigverify_forwarded(&self) {
        debug!("sigverify: forwarded");
    }
    fn sigverify_rejected(&self, reason: &'static str) {
        debug!("sigverify: rejected reason={}", reason);
    }
    fn sequencer_collected(&self, n: usize) {
        debug!("sequencer: collected {}", n);
    }
    fn sequencer_transactions_emitted(&self, n: usize) {
        debug!("sequencer: emitted {} transactions", n);
    }
    fn executor_results_sent(&self, n: usize) {
        debug!("executor: sent {} results", n);
    }
    fn executor_results_send_failed(&self, kind: &'static str) {
        debug!("executor: send failed kind={}", kind);
    }
    fn executor_missing_results(&self, kind: &'static str) {
        debug!("executor: missing results kind={}", kind);
    }
    fn settler_txs_settled(&self, n: usize) {
        debug!("settler: settled {}", n);
    }
}

// ---------------------------------------------------------------------------
// PrometheusMetrics — enabled via --metrics; writes to global registry.
// ---------------------------------------------------------------------------

use contra_metrics::{counter_vec, init_metrics};

// Counters
counter_vec!(
    DEDUP_RECEIVED,
    "contra_dedup_received_total",
    "Transactions received by dedup",
    &[]
);
counter_vec!(
    DEDUP_FORWARDED,
    "contra_dedup_forwarded_total",
    "Transactions forwarded by dedup",
    &[]
);
counter_vec!(
    DEDUP_DROPPED_DUP,
    "contra_dedup_dropped_duplicate_total",
    "Transactions dropped as duplicates",
    &[]
);
counter_vec!(
    DEDUP_DROPPED_UNK_BH,
    "contra_dedup_dropped_unknown_bh_total",
    "Transactions dropped for unknown blockhash",
    &[]
);
counter_vec!(
    SIGVERIFY_FORWARDED,
    "contra_sigverify_forwarded_total",
    "Transactions forwarded by sigverify",
    &[]
);
counter_vec!(
    SIGVERIFY_REJECTED,
    "contra_sigverify_rejected_total",
    "Transactions rejected by sigverify",
    &["reason"]
);
counter_vec!(
    SEQUENCER_COLLECTED,
    "contra_sequencer_collected_total",
    "Transactions collected by sequencer",
    &[]
);
counter_vec!(
    SEQUENCER_TXS_EMITTED,
    "contra_sequencer_transactions_emitted_total",
    "Transactions emitted by sequencer",
    &[]
);
counter_vec!(
    EXECUTOR_RESULTS_SENT,
    "contra_executor_results_sent_total",
    "Execution results sent to settler",
    &[]
);
counter_vec!(
    EXECUTOR_RESULTS_SEND_FAILED,
    "contra_executor_results_send_failed_total",
    "Failed to send execution results",
    &["kind"]
);
counter_vec!(
    EXECUTOR_MISSING_RESULTS,
    "contra_executor_missing_results_total",
    "Missing execution results",
    &["kind"]
);
counter_vec!(
    SETTLER_TXS_SETTLED,
    "contra_settler_txs_settled_total",
    "Transactions settled to DB",
    &[]
);

// Gauges
// Histograms — registered directly so we can specify custom buckets (in seconds).

pub struct PrometheusMetrics;

impl StageMetrics for PrometheusMetrics {
    fn dedup_received(&self) {
        DEDUP_RECEIVED.with_label_values(&[] as &[&str]).inc();
    }
    fn dedup_forwarded(&self) {
        DEDUP_FORWARDED.with_label_values(&[] as &[&str]).inc();
    }
    fn dedup_dropped_duplicate(&self) {
        DEDUP_DROPPED_DUP.with_label_values(&[] as &[&str]).inc();
    }
    fn dedup_dropped_unknown_blockhash(&self) {
        DEDUP_DROPPED_UNK_BH.with_label_values(&[] as &[&str]).inc();
    }
    fn sigverify_forwarded(&self) {
        SIGVERIFY_FORWARDED.with_label_values(&[] as &[&str]).inc();
    }
    fn sigverify_rejected(&self, reason: &'static str) {
        SIGVERIFY_REJECTED.with_label_values(&[reason]).inc();
    }
    fn sequencer_collected(&self, n: usize) {
        SEQUENCER_COLLECTED
            .with_label_values(&[] as &[&str])
            .inc_by(n as f64);
    }
    fn sequencer_transactions_emitted(&self, n: usize) {
        SEQUENCER_TXS_EMITTED
            .with_label_values(&[] as &[&str])
            .inc_by(n as f64);
    }
    fn executor_results_sent(&self, n: usize) {
        EXECUTOR_RESULTS_SENT
            .with_label_values(&[] as &[&str])
            .inc_by(n as f64);
    }
    fn executor_results_send_failed(&self, kind: &'static str) {
        EXECUTOR_RESULTS_SEND_FAILED
            .with_label_values(&[kind])
            .inc();
    }
    fn executor_missing_results(&self, kind: &'static str) {
        EXECUTOR_MISSING_RESULTS.with_label_values(&[kind]).inc();
    }
    fn settler_txs_settled(&self, n: usize) {
        SETTLER_TXS_SETTLED
            .with_label_values(&[] as &[&str])
            .inc_by(n as f64);
    }
}

/// Force-initialise all metric statics so they appear in /metrics from startup.
pub fn init_prometheus_metrics() {
    init_metrics!(
        DEDUP_RECEIVED,
        DEDUP_FORWARDED,
        DEDUP_DROPPED_DUP,
        DEDUP_DROPPED_UNK_BH,
        SIGVERIFY_FORWARDED,
        SIGVERIFY_REJECTED,
        SEQUENCER_COLLECTED,
        SEQUENCER_TXS_EMITTED,
        EXECUTOR_RESULTS_SENT,
        EXECUTOR_RESULTS_SEND_FAILED,
        EXECUTOR_MISSING_RESULTS,
        SETTLER_TXS_SETTLED
    );
    // Force histogram statics too
}
