use contra_metrics::{counter_vec, gauge_vec, histogram_vec};

// ---------------------------------------------------------------------------
// Indexer metrics
// ---------------------------------------------------------------------------

counter_vec!(
    INDEXER_SLOTS_PROCESSED,
    "contra_indexer_slots_processed_total",
    "Total slots checkpointed by the indexer",
    &["program_type"]
);

counter_vec!(
    INDEXER_TRANSACTIONS_SAVED,
    "contra_indexer_transactions_saved_total",
    "Total transactions saved to the database",
    &["program_type"]
);

counter_vec!(
    INDEXER_MINTS_SAVED,
    "contra_indexer_mints_saved_total",
    "Total mints upserted to the database",
    &["program_type"]
);

counter_vec!(
    INDEXER_SLOT_SAVE_ERRORS,
    "contra_indexer_slot_save_errors_total",
    "Total slot save errors (mints or transactions)",
    &["program_type"]
);

gauge_vec!(
    INDEXER_CURRENT_SLOT,
    "contra_indexer_current_slot",
    "Latest slot successfully checkpointed",
    &["program_type"]
);

counter_vec!(
    INDEXER_RPC_ERRORS,
    "contra_indexer_rpc_errors_total",
    "Total RPC errors in datasource layer",
    &["program_type", "error_type"]
);

// ---------------------------------------------------------------------------
// Operator metrics
// ---------------------------------------------------------------------------

counter_vec!(
    OPERATOR_TRANSACTIONS_FETCHED,
    "contra_operator_transactions_fetched_total",
    "Total transactions fetched from the database",
    &["program_type"]
);

counter_vec!(
    OPERATOR_TRANSACTIONS_SUBMITTED,
    "contra_operator_transactions_submitted_total",
    "Total transactions submitted to blockchain",
    &["program_type", "status"]
);

counter_vec!(
    OPERATOR_DB_UPDATES,
    "contra_operator_db_updates_total",
    "Total transaction status DB updates",
    &["program_type", "status"]
);

counter_vec!(
    OPERATOR_DB_UPDATE_ERRORS,
    "contra_operator_db_update_errors_total",
    "Total transaction status DB update errors",
    &["program_type"]
);

histogram_vec!(
    OPERATOR_RPC_SEND_DURATION,
    "contra_operator_rpc_send_duration_seconds",
    "Duration of RPC send_and_confirm calls",
    &["program_type", "result"]
);

pub fn init() {
    contra_metrics::init_metrics!(
        INDEXER_SLOTS_PROCESSED,
        INDEXER_TRANSACTIONS_SAVED,
        INDEXER_MINTS_SAVED,
        INDEXER_SLOT_SAVE_ERRORS,
        INDEXER_CURRENT_SLOT,
        INDEXER_RPC_ERRORS,
        OPERATOR_TRANSACTIONS_FETCHED,
        OPERATOR_TRANSACTIONS_SUBMITTED,
        OPERATOR_DB_UPDATES,
        OPERATOR_DB_UPDATE_ERRORS,
        OPERATOR_RPC_SEND_DURATION,
    );
}
