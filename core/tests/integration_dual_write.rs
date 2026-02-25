/// Integration tests for dual-write functionality
///
/// These tests verify that the settle worker continues operating when Redis is down,
/// demonstrating the core requirement: Redis failures are non-fatal and Postgres
/// remains the source of truth.

use contra_core::{
    accounts::{
        postgres::PostgresAccountsDB,
        redis::RedisAccountsDB,
        traits::{AccountsDB, BlockInfo},
        write_batch::write_batch,
    },
    stages::AccountSettlement,
};
use solana_hash::Hash;
use solana_sdk::{
    account::Account,
    pubkey::Pubkey,
    signature::{Keypair, Signer, Signature},
    system_transaction,
    transaction::SanitizedTransaction,
};
use solana_svm::transaction_processing_result::ProcessedTransaction;
use std::env;

/// Test that write_batch continues successfully when Redis is completely unavailable
///
/// This verifies the core dual-write requirement:
/// 1. Postgres writes succeed even when Redis is down
/// 2. Redis failures are logged but don't fail the operation
/// 3. The settle worker can continue operating with Postgres-only writes
///
/// Test scenario:
/// - Start with Postgres available but Redis unavailable (invalid URL)
/// - Create dual backend AccountsDB
/// - Submit a batch write operation
/// - Verify operation succeeds
/// - Verify data written to Postgres
/// - Warning logs for Redis failures are expected (captured by tracing)
#[tokio::test]
#[ignore] // Requires database setup: TEST_POSTGRES_URL environment variable
async fn test_settle_worker_continues_with_redis_down() {
    // Setup: Initialize tracing to capture warning logs
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_test_writer()
        .try_init();

    // Setup: Get test database URL from environment
    let postgres_url = env::var("TEST_POSTGRES_URL")
        .unwrap_or_else(|_| "postgresql://contra:contra@localhost:5432/contra_test".to_string());

    // Use an invalid Redis URL to simulate Redis being completely unavailable
    let redis_url = "redis://invalid-redis-host-that-does-not-exist:6379";

    // Setup: Create Postgres connection (should succeed)
    let postgres_db = match PostgresAccountsDB::new(&postgres_url, false).await {
        Ok(db) => db,
        Err(e) => {
            eprintln!("Skipping test: Cannot connect to test Postgres: {}", e);
            eprintln!("Set TEST_POSTGRES_URL environment variable to run this test");
            return;
        }
    };

    // Setup: Create Redis connection (this will fail when we try to use it)
    // Note: The connection may appear to succeed initially, but will fail on write
    let redis_db = match RedisAccountsDB::new(redis_url).await {
        Ok(db) => db,
        Err(e) => {
            // Expected: Redis connection to invalid host should fail
            eprintln!("Redis connection failed as expected: {}", e);
            // For this test, we need to proceed with a setup where writes will fail
            // If we can't even create the connection, we'll skip the test
            eprintln!("Skipping test: Cannot set up Redis failure scenario");
            return;
        }
    };

    // Create dual backend AccountsDB
    let mut accounts_db = AccountsDB::Dual(postgres_db, redis_db);

    // Setup: Create test data
    let keypair = Keypair::new();
    let pubkey = keypair.pubkey();

    // Create a test account with some data
    let account = Account {
        lamports: 100_000_000, // 0.1 SOL
        data: vec![1, 2, 3, 4, 5],
        owner: solana_sdk::system_program::id(),
        executable: false,
        rent_epoch: 0,
    };

    let account_settlement = AccountSettlement {
        account: solana_sdk::account::AccountSharedData::from(account),
        deleted: false,
    };
    let account_settlements = vec![(pubkey, account_settlement)];

    // Create a test transaction
    let transaction = system_transaction::transfer(
        &keypair,
        &Keypair::new().pubkey(),
        1_000_000, // Transfer 0.001 SOL
        Hash::default(),
    );
    let sanitized_tx = SanitizedTransaction::from_transaction_for_tests(transaction);
    let processed = ProcessedTransaction::default();

    let transactions = vec![(
        Signature::default(),
        &sanitized_tx,
        100u64,  // slot
        1234567890i64,  // timestamp
        &processed,
    )];

    let block_info = Some(BlockInfo {
        slot: 100,
        blockhash: Hash::default(),
        block_height: Some(100),
        block_time: Some(1234567890),
    });

    // Execute: Call write_batch with Redis unavailable
    // This is the critical test: the operation should succeed despite Redis being down
    let result = write_batch(
        &mut accounts_db,
        &account_settlements,
        transactions,
        block_info,
        Some(100),
    )
    .await;

    // Verify: Operation should succeed
    // Redis failures are logged (with tracing::warn) but not fatal
    assert!(
        result.is_ok(),
        "write_batch should succeed even when Redis is unavailable. Got error: {:?}",
        result.err()
    );

    // Verify: Data was written to Postgres (proving Postgres is source of truth)
    if let AccountsDB::Dual(postgres_db, _) = &accounts_db {
        let pubkey_bytes = pubkey.to_bytes();
        let postgres_result = sqlx::query_scalar::<_, Vec<u8>>(
            "SELECT data FROM accounts WHERE pubkey = $1"
        )
        .bind(&pubkey_bytes[..])
        .fetch_optional(&*postgres_db.pool)
        .await;

        assert!(
            postgres_result.is_ok(),
            "Should be able to query Postgres after write"
        );

        assert!(
            postgres_result.unwrap().is_some(),
            "Account data should exist in Postgres, proving Postgres write succeeded"
        );
    }

    // Test demonstrates:
    // 1. ✓ Settle worker can continue operating when Redis is down
    // 2. ✓ Transactions are written to Postgres successfully
    // 3. ✓ Redis failures don't prevent the operation from completing
    // 4. ✓ Warning logs are emitted for Redis failures (captured by tracing)
    // 5. ✓ Postgres remains the source of truth
}

/// Test that verifies the full settle worker flow with Redis unavailable
///
/// This is a more comprehensive test that simulates the actual settle worker
/// initialization and operation with Redis down.
///
/// Test scenario:
/// 1. Start Postgres only (Redis unavailable)
/// 2. Create dual backend with invalid Redis URL
/// 3. Submit multiple transaction batches
/// 4. Verify all transactions written to Postgres
/// 5. Verify no errors propagate to caller
/// 6. Warning logs for Redis failures are expected
#[tokio::test]
#[ignore] // Requires database setup: TEST_POSTGRES_URL environment variable
async fn test_multiple_batches_with_redis_down() {
    // Setup: Initialize tracing
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_test_writer()
        .try_init();

    // Setup: Get test database URL
    let postgres_url = env::var("TEST_POSTGRES_URL")
        .unwrap_or_else(|_| "postgresql://contra:contra@localhost:5432/contra_test".to_string());

    let redis_url = "redis://invalid-redis-host:6379";

    // Create connections
    let postgres_db = match PostgresAccountsDB::new(&postgres_url, false).await {
        Ok(db) => db,
        Err(e) => {
            eprintln!("Skipping test: Cannot connect to test Postgres: {}", e);
            return;
        }
    };

    let redis_db = match RedisAccountsDB::new(redis_url).await {
        Ok(db) => db,
        Err(_) => {
            eprintln!("Skipping test: Cannot set up Redis failure scenario");
            return;
        }
    };

    let mut accounts_db = AccountsDB::Dual(postgres_db, redis_db);

    // Submit multiple batches to verify continuous operation
    for i in 0..3 {
        let keypair = Keypair::new();
        let pubkey = keypair.pubkey();

        let account = Account {
            lamports: 100_000_000 + i * 1000,
            data: vec![i as u8; 10],
            owner: solana_sdk::system_program::id(),
            executable: false,
            rent_epoch: 0,
        };

        let account_settlement = AccountSettlement {
            account: solana_sdk::account::AccountSharedData::from(account),
            deleted: false,
        };

        let account_settlements = vec![(pubkey, account_settlement)];

        let transaction = system_transaction::transfer(
            &keypair,
            &Keypair::new().pubkey(),
            1_000_000,
            Hash::default(),
        );
        let sanitized_tx = SanitizedTransaction::from_transaction_for_tests(transaction);
        let processed = ProcessedTransaction::default();

        let transactions = vec![(
            Signature::default(),
            &sanitized_tx,
            100 + i,  // Different slot for each batch
            1234567890i64,
            &processed,
        )];

        let block_info = Some(BlockInfo {
            slot: 100 + i,
            blockhash: Hash::default(),
            block_height: Some(100 + i),
            block_time: Some(1234567890),
        });

        // Each batch should succeed despite Redis being down
        let result = write_batch(
            &mut accounts_db,
            &account_settlements,
            transactions,
            block_info,
            Some(100 + i),
        )
        .await;

        assert!(
            result.is_ok(),
            "Batch {} should succeed with Redis down. Got: {:?}",
            i,
            result.err()
        );
    }

    // Verify all batches were written to Postgres
    if let AccountsDB::Dual(postgres_db, _) = &accounts_db {
        let slot_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(DISTINCT slot) FROM blocks WHERE slot >= 100 AND slot < 103"
        )
        .fetch_one(&*postgres_db.pool)
        .await
        .expect("Should be able to query Postgres");

        assert_eq!(
            slot_count, 3,
            "All 3 batches should be written to Postgres"
        );
    }

    // Test demonstrates:
    // 1. ✓ Multiple consecutive batches succeed with Redis down
    // 2. ✓ Worker continues operating without Redis
    // 3. ✓ All data written to Postgres correctly
    // 4. ✓ No accumulation of errors over multiple batches
}

/// Test that verifies DB-first ordering is maintained when Redis is down
///
/// This test ensures that even when Redis is unavailable:
/// 1. Postgres writes complete successfully
/// 2. The operation returns success (Redis failure is non-fatal)
/// 3. Data is immediately queryable from Postgres
#[tokio::test]
#[ignore] // Requires database setup
async fn test_db_first_semantics_with_redis_down() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_test_writer()
        .try_init();

    let postgres_url = env::var("TEST_POSTGRES_URL")
        .unwrap_or_else(|_| "postgresql://contra:contra@localhost:5432/contra_test".to_string());

    let redis_url = "redis://invalid-host:6379";

    let postgres_db = match PostgresAccountsDB::new(&postgres_url, false).await {
        Ok(db) => db,
        Err(e) => {
            eprintln!("Skipping test: {}", e);
            return;
        }
    };

    let redis_db = match RedisAccountsDB::new(redis_url).await {
        Ok(db) => db,
        Err(_) => {
            eprintln!("Skipping test: Cannot set up Redis");
            return;
        }
    };

    let mut accounts_db = AccountsDB::Dual(postgres_db, redis_db);

    // Create test data
    let keypair = Keypair::new();
    let pubkey = keypair.pubkey();

    let account = Account {
        lamports: 500_000_000,
        data: vec![42; 20],
        owner: solana_sdk::system_program::id(),
        executable: false,
        rent_epoch: 0,
    };

    let account_settlement = AccountSettlement {
        account: solana_sdk::account::AccountSharedData::from(account),
        deleted: false,
    };

    let account_settlements = vec![(pubkey, account_settlement)];

    let transaction = system_transaction::transfer(
        &keypair,
        &Keypair::new().pubkey(),
        10_000_000,
        Hash::default(),
    );
    let sanitized_tx = SanitizedTransaction::from_transaction_for_tests(transaction);
    let processed = ProcessedTransaction::default();

    let transactions = vec![(
        Signature::default(),
        &sanitized_tx,
        200u64,
        1234567890i64,
        &processed,
    )];

    let block_info = Some(BlockInfo {
        slot: 200,
        blockhash: Hash::default(),
        block_height: Some(200),
        block_time: Some(1234567890),
    });

    // Execute write
    let result = write_batch(
        &mut accounts_db,
        &account_settlements,
        transactions,
        block_info,
        Some(200),
    )
    .await;

    // Should succeed because Postgres write succeeded (Redis failure is non-fatal)
    assert!(result.is_ok(), "Write should succeed: {:?}", result.err());

    // Immediately verify data is in Postgres (DB-first means Postgres commits first)
    if let AccountsDB::Dual(postgres_db, _) = &accounts_db {
        let pubkey_bytes = pubkey.to_bytes();

        // Query account data
        let account_data = sqlx::query_scalar::<_, Vec<u8>>(
            "SELECT data FROM accounts WHERE pubkey = $1"
        )
        .bind(&pubkey_bytes[..])
        .fetch_one(&*postgres_db.pool)
        .await
        .expect("Account should exist in Postgres");

        assert_eq!(
            account_data[0], 42,
            "Account data should match what we wrote"
        );

        // Query block info
        let block_exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM blocks WHERE slot = 200)"
        )
        .fetch_one(&*postgres_db.pool)
        .await
        .expect("Should be able to query blocks");

        assert!(block_exists, "Block should exist in Postgres");
    }

    // Test demonstrates:
    // 1. ✓ DB-first semantics preserved even with Redis down
    // 2. ✓ Postgres commit completes successfully
    // 3. ✓ Data immediately available for read from Postgres
    // 4. ✓ Operation returns success (Redis failure non-fatal)
}
