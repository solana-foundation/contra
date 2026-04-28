//! Bootstrap-time validation for `contra_indexer::indexer::run`.
//!
//! Covers the misconfiguration branch in
//! `contra_indexer::indexer::run`: when `program_type = Escrow` but no
//! `escrow_instance_id` is set, startup reconciliation has nothing to
//! anchor against, so `run` must bail with an `InvalidPubkey` error
//! (wrapped in `IndexerError::Reconciliation`).
//!
//! The existing `ContraIndexerConfig::validate()` method would also catch
//! this mismatch, but the `run` fast-path guard matters on its own because
//! production TOML load skips `validate()` if the user bypasses it in
//! custom bootstrapping. This test exercises the in-line guard specifically.

use {
    contra_indexer::{
        config::{BackfillConfig, ReconciliationConfig},
        error::{IndexerError, ReconciliationError},
        indexer::run,
        ContraIndexerConfig, DatasourceType, IndexerConfig, PostgresConfig, ProgramType,
        StorageType,
    },
    testcontainers::runners::AsyncRunner,
    testcontainers_modules::postgres::Postgres,
};

#[tokio::test(flavor = "multi_thread")]
async fn run_rejects_escrow_without_instance_id() {
    let pg_container = Postgres::default()
        .with_db_name("bootstrap_validation")
        .with_user("postgres")
        .with_password("password")
        .start()
        .await
        .expect("postgres container must start");
    let pg_host = pg_container.get_host().await.unwrap();
    let pg_port = pg_container.get_host_port_ipv4(5432).await.unwrap();
    let db_url = format!(
        "postgres://postgres:password@{}:{}/bootstrap_validation",
        pg_host, pg_port
    );

    let common_config = ContraIndexerConfig {
        program_type: ProgramType::Escrow,
        storage_type: StorageType::Postgres,
        rpc_url: "http://127.0.0.1:1".to_string(),
        source_rpc_url: None,
        postgres: PostgresConfig {
            database_url: db_url,
            max_connections: 2,
        },
        // Deliberately missing: this is the invariant under test.
        escrow_instance_id: None,
    };

    let indexer_config = IndexerConfig {
        datasource_type: DatasourceType::RpcPolling,
        rpc_polling: None,
        yellowstone: None,
        backfill: BackfillConfig {
            enabled: false,
            exit_after_backfill: false,
            rpc_url: "http://127.0.0.1:1".to_string(),
            batch_size: 100,
            max_gap_slots: 1_000,
            start_slot: None,
        },
        reconciliation: ReconciliationConfig::default(),
    };

    let result = run(common_config, indexer_config, None).await;

    let err = result.expect_err(
        "run() must reject Escrow program_type with no escrow_instance_id before touching the datasource",
    );
    match err {
        IndexerError::Reconciliation(ReconciliationError::InvalidPubkey { pubkey, reason }) => {
            assert_eq!(pubkey, "<missing>");
            assert!(
                reason.contains("escrow_instance_id"),
                "reason must mention the missing config field, got: {reason}"
            );
        }
        other => panic!(
            "expected IndexerError::Reconciliation(InvalidPubkey{{..}}) for missing escrow_instance_id, got: {other:?}"
        ),
    }
}
