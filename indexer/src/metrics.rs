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

gauge_vec!(
    INDEXER_CHAIN_TIP_SLOT,
    "contra_indexer_chain_tip_slot",
    "Latest slot on the Solana chain as seen by the datasource",
    &["program_type"]
);

gauge_vec!(
    INDEXER_BACKFILL_SLOTS_REMAINING,
    "contra_indexer_backfill_slots_remaining",
    "Remaining slots to backfill (0 when not backfilling)",
    &["program_type"]
);

counter_vec!(
    INDEXER_DATASOURCE_RECONNECTS,
    "contra_indexer_datasource_reconnects_total",
    "Total Yellowstone gRPC reconnections",
    &["program_type"]
);

histogram_vec!(
    INDEXER_SLOT_PROCESSING_DURATION,
    "contra_indexer_slot_processing_duration_seconds",
    "Time to process and checkpoint a slot",
    &["program_type"]
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

counter_vec!(
    OPERATOR_TRANSACTION_ERRORS,
    "contra_operator_transaction_errors_total",
    "Total transaction errors by reason (includes retried errors)",
    &["program_type", "error_reason"]
);

counter_vec!(
    OPERATOR_MINTS_SENT,
    "contra_operator_mints_sent_total",
    "Total mint transactions successfully confirmed",
    &["program_type"]
);

gauge_vec!(
    OPERATOR_BACKLOG_DEPTH,
    "contra_operator_backlog_depth",
    "Number of pending transactions in the database",
    &["program_type"]
);

gauge_vec!(
    FEEPAYER_BALANCE_LAMPORTS,
    "contra_feepayer_balance_lamports",
    "Current SOL balance of the escrow operator feepayer wallet in lamports",
    &["program_type"]
);

pub fn init_labels(program_type: &str) {
    INDEXER_MINTS_SAVED.with_label_values(&[program_type]);
    INDEXER_TRANSACTIONS_SAVED.with_label_values(&[program_type]);
    INDEXER_SLOT_SAVE_ERRORS.with_label_values(&[program_type]);
    INDEXER_SLOTS_PROCESSED.with_label_values(&[program_type]);
    INDEXER_DATASOURCE_RECONNECTS.with_label_values(&[program_type]);

    INDEXER_CURRENT_SLOT.with_label_values(&[program_type]);
    INDEXER_CHAIN_TIP_SLOT.with_label_values(&[program_type]);
    INDEXER_BACKFILL_SLOTS_REMAINING.with_label_values(&[program_type]);
    INDEXER_SLOT_PROCESSING_DURATION.with_label_values(&[program_type]);

    for error_type in &["stream", "get_slots", "get_block"] {
        INDEXER_RPC_ERRORS.with_label_values(&[program_type, error_type]);
    }

    OPERATOR_TRANSACTIONS_FETCHED.with_label_values(&[program_type]);
    OPERATOR_MINTS_SENT.with_label_values(&[program_type]);
    OPERATOR_DB_UPDATE_ERRORS.with_label_values(&[program_type]);

    for status in &["Pending", "Processing", "Completed", "Failed"] {
        OPERATOR_DB_UPDATES.with_label_values(&[program_type, status]);
    }

    for result in &["success", "failure", "error"] {
        OPERATOR_RPC_SEND_DURATION.with_label_values(&[program_type, result]);
    }

    for error_reason in &[
        "build_error",
        "max_retries_exceeded",
        "rpc_send_error",
        "invalid_smt_proof",
        "invalid_nonce_for_tree_index",
        "mint_not_initialized",
        "confirmation_timeout_non_idempotent",
        "confirmation_timeout",
        "program_error",
        "confirmation_error",
    ] {
        OPERATOR_TRANSACTION_ERRORS.with_label_values(&[program_type, error_reason]);
    }

    OPERATOR_BACKLOG_DEPTH.with_label_values(&[program_type]);
    FEEPAYER_BALANCE_LAMPORTS.with_label_values(&[program_type]);
}

pub fn init() {
    contra_metrics::init_metrics!(
        INDEXER_SLOTS_PROCESSED,
        INDEXER_TRANSACTIONS_SAVED,
        INDEXER_MINTS_SAVED,
        INDEXER_SLOT_SAVE_ERRORS,
        INDEXER_CURRENT_SLOT,
        INDEXER_RPC_ERRORS,
        INDEXER_CHAIN_TIP_SLOT,
        INDEXER_BACKFILL_SLOTS_REMAINING,
        INDEXER_DATASOURCE_RECONNECTS,
        INDEXER_SLOT_PROCESSING_DURATION,
        OPERATOR_TRANSACTIONS_FETCHED,
        OPERATOR_DB_UPDATES,
        OPERATOR_DB_UPDATE_ERRORS,
        OPERATOR_RPC_SEND_DURATION,
        OPERATOR_TRANSACTION_ERRORS,
        OPERATOR_MINTS_SENT,
        OPERATOR_BACKLOG_DEPTH,
        FEEPAYER_BALANCE_LAMPORTS,
    );
}
