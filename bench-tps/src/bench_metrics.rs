use private_channel_metrics::{counter_vec, init_metrics};

/// Label name used on all per-flow counters.
pub const LABEL_FLOW: &str = "flow";

counter_vec!(
    BENCH_SENT_TOTAL,
    "private_channel_bench_tps_sent_total",
    "Total transactions sent by the bench",
    &[LABEL_FLOW]
);

counter_vec!(
    BENCH_LANDED_TOTAL,
    "private_channel_bench_tps_landed_total",
    "Total transactions observed as landed by the node",
    &[LABEL_FLOW]
);

counter_vec!(
    BENCH_WITHDRAW_BURN_CONFIRMED_TOTAL,
    "private_channel_bench_tps_withdraw_burn_confirmed_total",
    "Total withdraw-burn transactions confirmed on PrivateChannel",
    &[LABEL_FLOW]
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
