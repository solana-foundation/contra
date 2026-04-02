use contra_metrics::{counter_vec, init_metrics};

counter_vec!(
    BENCH_SENT_TOTAL,
    "contra_bench_tps_sent_total",
    "Total transactions sent by the bench",
    &["flow"]
);

counter_vec!(
    BENCH_LANDED_TOTAL,
    "contra_bench_tps_landed_total",
    "Total transactions observed as landed by the node",
    &["flow"]
);

counter_vec!(
    BENCH_WITHDRAW_BURN_CONFIRMED_TOTAL,
    "contra_bench_tps_withdraw_burn_confirmed_total",
    "Total withdraw-burn transactions confirmed on L2",
    &["flow"]
);

pub fn bench_metrics_init() {
    init_metrics!(
        BENCH_SENT_TOTAL,
        BENCH_LANDED_TOTAL,
        BENCH_WITHDRAW_BURN_CONFIRMED_TOTAL
    );
}

pub const FLOW_TRANSFER: &str = "transfer";
pub const FLOW_DEPOSIT: &str = "deposit";
pub const FLOW_WITHDRAW: &str = "withdraw";
