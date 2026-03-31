use contra_metrics::{counter_vec, init_metrics};

counter_vec!(
    BENCH_SENT_TOTAL,
    "contra_bench_tps_sent_total",
    "Total transactions sent by the bench",
    &[]
);

counter_vec!(
    BENCH_LANDED_TOTAL,
    "contra_bench_tps_landed_total",
    "Total transactions observed as landed by the node",
    &[]
);

pub fn init() {
    init_metrics!(BENCH_SENT_TOTAL, BENCH_LANDED_TOTAL);
}

pub const NO_LABELS: [&str; 0] = [];
