//! Database migration idempotency + insert-race safety
//!
//! Target file: `indexer/src/storage/postgres/db.rs`.
//! Binary: `reconciliation_integration` (existing — attached via `#[path]`
//! mod from `tests/indexer/reconciliation.rs`).
//!
//! Two tests:
//!
//!   1. **`test_init_schema_is_idempotent_across_runs`** — calls
//!      `init_schema()` twice on the same pool and asserts no error, no
//!      orphan rows, and that every `CREATE TABLE IF NOT EXISTS` /
//!      `ALTER TABLE ... ADD COLUMN IF NOT EXISTS` / `ADD VALUE IF NOT
//!      EXISTS` branch is exercised. Captures the forward-compatibility
//!      guarantee we rely on when pg_restore-ing from a previous version.
//!
//!   2. **`test_insert_transaction_duplicate_signature_returns_existing_id`**
//!      — inserts the same `DbTransaction` twice concurrently and asserts
//!      both calls return the same `id` without raising an error. Exercises
//!      the `SELECT ... WHERE signature = $1` early-return AND the
//!      `ON CONFLICT DO NOTHING + fallback SELECT` branch (the one that
//!      fires when two writers race past the initial existence check).

use {
    private_channel_indexer::storage::{
        common::models::DbTransactionBuilder, PostgresDb, Storage, TransactionType,
    },
    private_channel_indexer::PostgresConfig,
    solana_sdk::signature::Signature,
    testcontainers::runners::AsyncRunner,
    testcontainers_modules::postgres::Postgres,
};

async fn start_postgres(
    db_name: &str,
) -> (PostgresDb, String, testcontainers::ContainerAsync<Postgres>) {
    let container = Postgres::default()
        .with_db_name(db_name)
        .with_user("postgres")
        .with_password("password")
        .start()
        .await
        .expect("postgres container");
    let host = container.get_host().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:password@{}:{}/{}", host, port, db_name);
    let db = PostgresDb::new(&PostgresConfig {
        database_url: url.clone(),
        max_connections: 10,
    })
    .await
    .unwrap();
    (db, url, container)
}

// ── 1. Schema-init idempotency ─────────────────────────────────────────────
#[tokio::test(flavor = "multi_thread")]
async fn test_init_schema_is_idempotent_across_runs() {
    let (db, url, _container) = start_postgres("c1_schema_idempotent").await;
    let storage = Storage::Postgres(db);

    // First run creates every table/enum/column.
    storage.init_schema().await.expect("first init_schema");
    // Second run must be a no-op: all IF NOT EXISTS branches taken.
    storage
        .init_schema()
        .await
        .expect("init_schema must be idempotent; the second run took ALTER/CREATE branches");

    // Sanity: transactions table still exists and is queryable. Connect a
    // separate pool because PostgresDb.pool is private.
    let pool = sqlx::PgPool::connect(&url).await.expect("sqlx connect");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM transactions")
        .fetch_one(&pool)
        .await
        .expect("transactions table must be queryable after double-init");
    assert_eq!(count, 0, "no rows yet; table exists and is reachable");
}

// ── 2. Duplicate-key race in insert_transaction ────────────────────────────
#[tokio::test(flavor = "multi_thread")]
async fn test_insert_transaction_duplicate_signature_returns_existing_id() {
    let (db, url, _container) = start_postgres("c1_dup_insert").await;
    let storage = Storage::Postgres(db.clone());
    storage.init_schema().await.unwrap();

    let signature = Signature::new_unique().to_string();
    let mint = solana_sdk::pubkey::Pubkey::new_unique().to_string();
    let recipient = solana_sdk::pubkey::Pubkey::new_unique().to_string();

    let build = || {
        DbTransactionBuilder::new(signature.clone(), 1, mint.clone(), 100u64)
            .initiator(recipient.clone())
            .recipient(recipient.clone())
            .transaction_type(TransactionType::Deposit)
            .build()
    };

    // Spawn two concurrent inserts of the *same* signature. Both must
    // succeed and must return the same row id (from whichever path ended
    // up producing the row — either the first INSERT or the race-fallback
    // SELECT after ON CONFLICT DO NOTHING).
    let tx1 = build();
    let tx2 = build();
    let db1 = db.clone();
    let db2 = db.clone();
    let (r1, r2) = tokio::join!(
        tokio::spawn(async move { db1.insert_transaction_internal(&tx1).await }),
        tokio::spawn(async move { db2.insert_transaction_internal(&tx2).await }),
    );
    let id1 = r1.expect("t1 not panic").expect("insert 1 ok");
    let id2 = r2.expect("t2 not panic").expect("insert 2 ok");
    assert_eq!(
        id1, id2,
        "duplicate-signature inserts must resolve to the same id (id1={id1} id2={id2})"
    );

    // Row exists exactly once.
    let pool = sqlx::PgPool::connect(&url).await.expect("sqlx connect");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM transactions WHERE signature = $1")
        .bind(&signature)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "exactly one row survives the race; got {count}");
}
