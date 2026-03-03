use once_cell::sync::Lazy;
use prometheus::{
    register_counter_vec, register_gauge_vec, register_histogram_vec, CounterVec, Encoder,
    GaugeVec, HistogramVec, TextEncoder,
};

// ---------------------------------------------------------------------------
// Indexer metrics
// ---------------------------------------------------------------------------

pub static INDEXER_SLOTS_PROCESSED: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "contra_indexer_slots_processed_total",
        "Total slots checkpointed by the indexer",
        &["program_type"]
    )
    .unwrap()
});

pub static INDEXER_TRANSACTIONS_SAVED: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "contra_indexer_transactions_saved_total",
        "Total transactions saved to the database",
        &["program_type"]
    )
    .unwrap()
});

pub static INDEXER_MINTS_SAVED: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "contra_indexer_mints_saved_total",
        "Total mints upserted to the database",
        &["program_type"]
    )
    .unwrap()
});

pub static INDEXER_SLOT_SAVE_ERRORS: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "contra_indexer_slot_save_errors_total",
        "Total slot save errors (mints or transactions)",
        &["program_type"]
    )
    .unwrap()
});

pub static INDEXER_CURRENT_SLOT: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "contra_indexer_current_slot",
        "Latest slot successfully checkpointed",
        &["program_type"]
    )
    .unwrap()
});

pub static INDEXER_RPC_ERRORS: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "contra_indexer_rpc_errors_total",
        "Total RPC errors in datasource layer",
        &["program_type", "error_type"]
    )
    .unwrap()
});

// ---------------------------------------------------------------------------
// Operator metrics
// ---------------------------------------------------------------------------

pub static OPERATOR_TRANSACTIONS_FETCHED: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "contra_operator_transactions_fetched_total",
        "Total transactions fetched from the database",
        &["program_type"]
    )
    .unwrap()
});

pub static OPERATOR_TRANSACTIONS_SUBMITTED: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "contra_operator_transactions_submitted_total",
        "Total transactions submitted to blockchain",
        &["program_type", "status"]
    )
    .unwrap()
});

pub static OPERATOR_DB_UPDATES: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "contra_operator_db_updates_total",
        "Total transaction status DB updates",
        &["program_type", "status"]
    )
    .unwrap()
});

pub static OPERATOR_DB_UPDATE_ERRORS: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "contra_operator_db_update_errors_total",
        "Total transaction status DB update errors",
        &["program_type"]
    )
    .unwrap()
});

pub static OPERATOR_RPC_SEND_DURATION: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "contra_operator_rpc_send_duration_seconds",
        "Duration of RPC send_and_confirm calls",
        &["program_type", "result"]
    )
    .unwrap()
});

pub fn init() {
    Lazy::force(&INDEXER_SLOTS_PROCESSED);
    Lazy::force(&INDEXER_TRANSACTIONS_SAVED);
    Lazy::force(&INDEXER_MINTS_SAVED);
    Lazy::force(&INDEXER_SLOT_SAVE_ERRORS);
    Lazy::force(&INDEXER_CURRENT_SLOT);
    Lazy::force(&INDEXER_RPC_ERRORS);
    Lazy::force(&OPERATOR_TRANSACTIONS_FETCHED);
    Lazy::force(&OPERATOR_TRANSACTIONS_SUBMITTED);
    Lazy::force(&OPERATOR_DB_UPDATES);
    Lazy::force(&OPERATOR_DB_UPDATE_ERRORS);
    Lazy::force(&OPERATOR_RPC_SEND_DURATION);
}

async fn metrics_handler() -> ([(axum::http::header::HeaderName, &'static str); 1], Vec<u8>) {
    let encoder = TextEncoder::new();
    let metric_families = prometheus::gather();
    let mut buffer = Vec::new();
    encoder.encode(&metric_families, &mut buffer).unwrap();
    (
        [(axum::http::header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        buffer,
    )
}

pub async fn start_metrics_server(port: u16) {
    let app = axum::Router::new().route("/metrics", axum::routing::get(metrics_handler));

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!("Metrics server listening on {}", addr);

    tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
        axum::serve(listener, app).await.unwrap();
    });
}
