//! Integration tests for batch atomicity — invariant C1:
//! A slot and all its account changes, transactions, and metadata MUST be written
//! as a single DB transaction. Either the whole slot commits or nothing does.
//!
//! Two tests verify this from complementary angles:
//!
//! 1. `test_write_batch_constraint_injection` — adds a CHECK constraint that forces
//!    `write_batch` to fail after accounts are written but before the block row is
//!    inserted, then asserts all prior writes in that batch were rolled back.
//!    This proves our code uses a real transaction.
//!
//! 2. `test_write_batch_process_kill_simulation` — opens a raw Postgres connection,
//!    manually BEGINs a transaction and writes partial slot data, then uses
//!    `pg_terminate_backend()` to forcibly kill that connection (identical to what
//!    Postgres sees when the OS sends SIGKILL to the Contra process), and asserts
//!    the partial data is gone.
//!    This proves the underlying mechanism works under real connection-kill conditions.

use {
    contra_core::{
        accounts::{traits::BlockInfo, AccountsDB},
        stages::AccountSettlement,
    },
    solana_sdk::{account::AccountSharedData, hash::Hash, pubkey::Pubkey},
    sqlx::{postgres::PgConnection, Connection},
    std::sync::Arc,
    testcontainers::runners::AsyncRunner,
    testcontainers_modules::postgres::Postgres,
};

fn slot_block_info(slot: u64) -> BlockInfo {
    BlockInfo {
        slot,
        blockhash: Hash::default(),
        previous_blockhash: Hash::default(),
        parent_slot: slot.saturating_sub(1),
        block_height: Some(slot),
        block_time: Some(0),
        transaction_signatures: vec![],
    }
}

fn bare_account(lamports: u64) -> AccountSharedData {
    AccountSharedData::new(lamports, 0, &Pubkey::default())
}

/// Test 1: constraint injection
///
/// Forces `write_batch` to fail mid-transaction by temporarily making block inserts
/// for slot 2 violate a CHECK constraint. Asserts that the accounts written earlier
/// in the same transaction were rolled back — no partial slot 2 data remains.
#[tokio::test(flavor = "multi_thread")]
async fn test_write_batch_constraint_injection() {
    let container = Postgres::default()
        .with_db_name("contra_node")
        .with_user("postgres")
        .with_password("password")
        .start()
        .await
        .expect("Failed to start PostgreSQL container");

    let url = format!(
        "postgres://postgres:password@{}:{}/contra_node",
        container.get_host().await.unwrap(),
        container.get_host_port_ipv4(5432).await.unwrap(),
    );

    let mut db = AccountsDB::new(&url, false)
        .await
        .expect("Failed to create AccountsDB");

    // Write slot 1 as a clean baseline.
    let pubkey_slot1 = Pubkey::new_unique();
    db.write_batch(
        &[(
            pubkey_slot1,
            AccountSettlement {
                account: bare_account(1_000_000),
                deleted: false,
            },
        )],
        vec![],
        Some(slot_block_info(1)),
        Some(1),
    )
    .await
    .expect("slot 1 write_batch must succeed");

    assert_eq!(db.get_latest_slot().await.unwrap(), 1);

    // Inject fault: any INSERT into blocks with slot = 2 will fail.
    // This simulates a mid-transaction failure after accounts have been written
    // but before the block row (which comes later in write_batch) is inserted.
    let pool = match &db {
        AccountsDB::Postgres(pg) => Arc::clone(&pg.pool),
        _ => panic!("Expected Postgres backend"),
    };
    sqlx::query("ALTER TABLE blocks ADD CONSTRAINT test_no_slot_2 CHECK (slot <> 2)")
        .execute(&*pool)
        .await
        .expect("Failed to add test constraint");

    // Attempt write_batch for slot 2 — the block insert will hit the constraint,
    // the error propagates out of the transaction, sqlx rolls everything back.
    let pubkey_slot2 = Pubkey::new_unique();
    let result = db
        .write_batch(
            &[(
                pubkey_slot2,
                AccountSettlement {
                    account: bare_account(2_000_000),
                    deleted: false,
                },
            )],
            vec![],
            Some(slot_block_info(2)),
            Some(2),
        )
        .await;

    assert!(
        result.is_err(),
        "write_batch must fail when the block insert violates the constraint"
    );

    // latest_slot is derived from MAX(slot) in blocks — slot 2 block was rolled back.
    assert_eq!(
        db.get_latest_slot().await.unwrap(),
        1,
        "latest_slot must still be 1; slot 2 block was never committed"
    );

    // The block row itself must not exist.
    assert!(
        db.get_block(2).await.is_none(),
        "slot 2 block must not exist after the rolled-back write_batch"
    );

    // pubkey_slot2's account was written to the accounts table BEFORE the block
    // insert failed. It must have been rolled back with the rest of the transaction.
    let accounts = db.get_accounts(&[pubkey_slot2]).await;
    assert!(
        accounts[0].is_none(),
        "pubkey_slot2 account must not exist — it was rolled back with the transaction"
    );

    // Slot 1 data must be completely intact.
    let accounts = db.get_accounts(&[pubkey_slot1]).await;
    assert!(
        accounts[0].is_some(),
        "pubkey_slot1 (slot 1 baseline) must still exist"
    );

    // Remove the constraint and confirm slot 2 can now be written cleanly,
    // proving the DB was left in a fully usable state.
    sqlx::query("ALTER TABLE blocks DROP CONSTRAINT test_no_slot_2")
        .execute(&*pool)
        .await
        .expect("Failed to drop test constraint");

    db.write_batch(
        &[(
            pubkey_slot2,
            AccountSettlement {
                account: bare_account(2_000_000),
                deleted: false,
            },
        )],
        vec![],
        Some(slot_block_info(2)),
        Some(2),
    )
    .await
    .expect("slot 2 write_batch must succeed after constraint is removed");

    assert_eq!(
        db.get_latest_slot().await.unwrap(),
        2,
        "DB must be at slot 2 after the clean write"
    );
}

/// Test 2: process kill simulation via `pg_terminate_backend`
///
/// Opens a raw Postgres connection (simulating the Contra process's DB connection),
/// manually BEGINs a transaction, and writes partial slot data directly — exactly
/// the state the DB is in when a settle is mid-flight. Then calls
/// `pg_terminate_backend(pid)` to forcibly kill that connection, which is what
/// Postgres sees when the OS sends SIGKILL to the application process. Asserts that
/// Postgres rolled back the in-flight transaction and no partial data remains.
#[tokio::test(flavor = "multi_thread")]
async fn test_write_batch_process_kill_simulation() {
    let container = Postgres::default()
        .with_db_name("contra_node")
        .with_user("postgres")
        .with_password("password")
        .start()
        .await
        .expect("Failed to start PostgreSQL container");

    let url = format!(
        "postgres://postgres:password@{}:{}/contra_node",
        container.get_host().await.unwrap(),
        container.get_host_port_ipv4(5432).await.unwrap(),
    );

    // Initialize the schema via AccountsDB (creates all tables).
    let db = AccountsDB::new(&url, false)
        .await
        .expect("Failed to create AccountsDB");

    // Write slot 1 as a clean baseline using write_batch.
    let pubkey_slot1 = Pubkey::new_unique();
    {
        let mut db_write = db.clone();
        db_write
            .write_batch(
                &[(
                    pubkey_slot1,
                    AccountSettlement {
                        account: bare_account(1_000_000),
                        deleted: false,
                    },
                )],
                vec![],
                Some(slot_block_info(1)),
                Some(1),
            )
            .await
            .expect("slot 1 write_batch must succeed");
    }

    assert_eq!(db.get_latest_slot().await.unwrap(), 1);

    // Open a raw connection — this represents the Contra process's DB connection
    // that is in the middle of a write_batch for slot 2.
    let mut victim = PgConnection::connect(&url)
        .await
        .expect("Failed to open victim connection");

    // Manually begin the transaction (as write_batch would via pool.begin()).
    sqlx::query("BEGIN")
        .execute(&mut victim)
        .await
        .expect("BEGIN must succeed");

    // Write partial slot 2 data: an account and a block row, but do NOT commit.
    // This mirrors the mid-flight state of write_batch when the process is killed.
    // The bytes content doesn't matter here — we're only asserting these rows
    // are absent after the connection is killed, not reading their values back.
    let pubkey_slot2 = Pubkey::new_unique();
    let dummy_bytes = vec![0u8; 32];

    sqlx::query("INSERT INTO accounts (pubkey, data) VALUES ($1, $2)")
        .bind(&pubkey_slot2.to_bytes()[..])
        .bind(&dummy_bytes[..])
        .execute(&mut victim)
        .await
        .expect("accounts INSERT must succeed inside the open transaction");

    sqlx::query("INSERT INTO blocks (slot, data) VALUES ($1, $2)")
        .bind(2i64)
        .bind(&dummy_bytes[..])
        .execute(&mut victim)
        .await
        .expect("blocks INSERT must succeed inside the open transaction");

    // Get the victim connection's backend PID so we can kill it from outside.
    let victim_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut victim)
        .await
        .expect("Failed to get pg_backend_pid");

    // Open a second connection (the "executioner") to terminate the victim.
    // This is equivalent to the OS sending SIGKILL to the Contra process:
    // Postgres detects the backend termination and rolls back the open transaction.
    let mut executioner = PgConnection::connect(&url)
        .await
        .expect("Failed to open executioner connection");

    sqlx::query("SELECT pg_terminate_backend($1)")
        .bind(victim_pid)
        .execute(&mut executioner)
        .await
        .expect("pg_terminate_backend must succeed");

    // The victim connection is now dead server-side. Drop it.
    drop(victim);

    // Give Postgres a moment to process the termination and roll back.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Verify: Postgres rolled back the in-flight transaction, leaving no partial data.

    // latest_slot is MAX(slot) from blocks — slot 2 block was rolled back.
    assert_eq!(
        db.get_latest_slot().await.unwrap(),
        1,
        "latest_slot must still be 1 after the simulated process kill"
    );

    // The block row must not exist.
    assert!(
        db.get_block(2).await.is_none(),
        "slot 2 block must not exist — Postgres rolled back on connection kill"
    );

    // The account written inside the killed transaction must not exist.
    let accounts = db.get_accounts(&[pubkey_slot2]).await;
    assert!(
        accounts[0].is_none(),
        "pubkey_slot2 must not exist — rolled back with the killed transaction"
    );

    // Slot 1 baseline must be fully intact.
    let accounts = db.get_accounts(&[pubkey_slot1]).await;
    assert!(
        accounts[0].is_some(),
        "pubkey_slot1 (slot 1 baseline) must still exist"
    );
}
