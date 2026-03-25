use contra_metrics::{counter_vec, gauge_vec, init_metrics};

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

gauge_vec!(
    BENCH_TPS_CURRENT,
    "contra_bench_tps_current_tps",
    "Instantaneous TPS observed by the node",
    &[]
);

pub fn init() {
    init_metrics!(BENCH_SENT_TOTAL, BENCH_LANDED_TOTAL, BENCH_TPS_CURRENT);
}

pub const NO_LABELS: [&str; 0] = [];
