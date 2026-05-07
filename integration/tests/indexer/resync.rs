//! Integration tests for [`ResyncService`].
//!
//! Verifies that resync correctly drops DB tables, recreates the schema, and
//! runs a backfill from a caller-supplied genesis slot.  Two scenarios:
//!
//! 1. `genesis_slot = current_slot` — no history to process, 0 or 1 slots
//!    backfilled, existing rows are purged, `run()` returns `Ok(())`.
//! 2. `genesis_slot = u64::MAX` — ahead of chain tip, `run()` returns an
//!    `Err` mentioning genesis_slot / current_slot.

#[path = "helpers/mod.rs"]
mod helpers;

use private_channel_indexer::{
    config::{BackfillConfig, ProgramType},
    indexer::{datasource::rpc_polling::rpc::RpcPoller, resync::ResyncService},
    storage::{PostgresDb, Storage},
    PostgresConfig,
};
use solana_sdk::commitment_config::CommitmentLevel;
use solana_transaction_status::UiTransactionEncoding;
use std::sync::Arc;
use test_utils::validator_helper::start_test_validator_no_geyser;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;

// ── helpers ───────────────────────────────────────────────────────────────────

async fn start_postgres_for_resync(
    db_name: &str,
) -> Result<
    (
        String,
        Arc<Storage>,
        testcontainers::ContainerAsync<Postgres>,
    ),
    Box<dyn std::error::Error>,
> {
    let container = Postgres::default()
        .with_db_name(db_name)
        .with_user("postgres")
        .with_password("password")
        .start()
        .await?;
    let host = container.get_host().await?;
    let port = container.get_host_port_ipv4(5432).await?;
    let db_url = format!("postgres://postgres:password@{}:{}/{}", host, port, db_name);

    let storage = Arc::new(Storage::Postgres(
        PostgresDb::new(&PostgresConfig {
            database_url: db_url.clone(),
            max_connections: 5,
        })
        .await?,
    ));
    storage.init_schema().await?;

    Ok((db_url, storage, container))
}

fn make_resync_service(rpc_url: String, storage: Arc<Storage>) -> ResyncService {
    let rpc_poller = Arc::new(RpcPoller::new(
        rpc_url.clone(),
        UiTransactionEncoding::Json,
        CommitmentLevel::Finalized,
    ));
    let backfill_config = BackfillConfig {
        enabled: true,
        exit_after_backfill: true,
        rpc_url,
        batch_size: 50,
        max_gap_slots: u64::MAX,
        start_slot: None,
    };
    ResyncService::new(
        storage,
        rpc_poller,
        ProgramType::Escrow,
        backfill_config,
        None,
    )
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// ResyncService drops all DB tables, recreates the schema, then runs a short
/// backfill (genesis_slot ≈ current_slot → very few slots to process).
/// After `run()` returns `Ok`, the transactions table must be empty.
#[tokio::test(flavor = "multi_thread")]
async fn test_resync_clears_db_and_returns_ok() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Resync: Clear DB and return Ok ===");

    let (test_validator, _faucet) = start_test_validator_no_geyser().await;
    let rpc_url = test_validator.rpc_url();

    let (db_url, storage, _container) = start_postgres_for_resync("resync_clear_test").await?;

    // Insert a dummy row so we can verify that resync wipes it.
    {
        let pool = sqlx::PgPool::connect(&db_url).await?;
        sqlx::query(
            "INSERT INTO transactions
             (signature, slot, initiator, recipient, mint, amount,
              transaction_type, status, created_at, updated_at)
             VALUES ('resync_sig_001', 1, 'test', 'test', 'mint_dummy', 100,
                     'deposit'::transaction_type, 'pending'::transaction_status,
                     NOW(), NOW())",
        )
        .execute(&pool)
        .await?;

        let count_before: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM transactions")
            .fetch_one(&pool)
            .await?;
        assert_eq!(count_before.0, 1, "Should have 1 row before resync");
        println!("  Rows before resync: {}", count_before.0);
    }

    // Fetch current slot so the backfill range is as small as possible.
    let current_slot = {
        let client = solana_client::rpc_client::RpcClient::new(rpc_url.clone());
        client.get_slot()?
    };
    println!("  Current slot used as genesis_slot: {}", current_slot);

    let service = make_resync_service(rpc_url.clone(), storage);

    // Run resync with a 60-second deadline; it should complete in < 5 s.
    tokio::time::timeout(
        std::time::Duration::from_secs(60),
        service.run(current_slot),
    )
    .await
    .expect("ResyncService::run timed out after 60 s")?;

    println!("  Resync returned Ok — verifying DB is empty");

    // Open a fresh pool to avoid any stale prepared-statement cache from before
    // the table drop / recreation.
    let pool_after = sqlx::PgPool::connect(&db_url).await?;
    let count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM transactions")
        .fetch_one(&pool_after)
        .await?;
    assert_eq!(
        count_after, 0,
        "Transactions table must be empty after resync"
    );
    println!("  Rows after resync: {} ✓", count_after);

    println!("=== Resync: Clear DB PASSED ===");
    Ok(())
}

/// ResyncService must return `Err` when `genesis_slot` is ahead of the
/// current chain tip.  The tables are still recreated (drop + init happens
/// before the slot validation), but the overall `run()` returns an error.
#[tokio::test(flavor = "multi_thread")]
async fn test_resync_rejects_future_genesis_slot() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Resync: Reject future genesis slot ===");

    let (test_validator, _faucet) = start_test_validator_no_geyser().await;
    let rpc_url = test_validator.rpc_url();

    let (_db_url, storage, _container) =
        start_postgres_for_resync("resync_future_slot_test").await?;

    let service = make_resync_service(rpc_url, storage);

    println!("  Calling run(u64::MAX) — expecting Err");
    let result = service.run(u64::MAX).await;

    assert!(
        result.is_err(),
        "ResyncService::run with a future genesis_slot must return Err, got Ok"
    );

    let err_msg = result.unwrap_err().to_string();
    println!("  Error received: {}", err_msg);
    // The error message must reference the slot mismatch.
    assert!(
        err_msg.contains("genesis_slot")
            || err_msg.contains("current_slot")
            || err_msg.contains("ahead")
            || err_msg.contains("invalid")
            || err_msg.contains("Invalid"),
        "Error should mention slot context, got: {}",
        err_msg
    );

    println!("=== Resync: Reject future genesis slot PASSED ===");
    Ok(())
}
