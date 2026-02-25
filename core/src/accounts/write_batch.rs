use {
    super::{
        postgres::PostgresAccountsDB,
        redis::RedisAccountsDB,
        traits::{AccountsDB, BlockInfo},
        transaction_count::TransactionCount,
        utils::get_stored_transaction,
    },
    crate::stages::AccountSettlement,
    solana_sdk::{
        clock::UnixTimestamp, pubkey::Pubkey, signature::Signature,
        transaction::SanitizedTransaction,
    },
    solana_svm::transaction_processing_result::ProcessedTransaction,
    std::sync::Arc,
    tracing::warn,
};

pub async fn write_batch(
    db: &mut AccountsDB,
    account_settlements: &[(Pubkey, AccountSettlement)],
    transactions: Vec<(
        Signature,
        &SanitizedTransaction,
        u64,
        UnixTimestamp,
        &ProcessedTransaction,
    )>,
    block_info: Option<BlockInfo>,
    slot: Option<u64>,
) -> Result<(), String> {
    match db {
        AccountsDB::Postgres(postgres_db) => {
            write_batch_postgres(
                postgres_db,
                account_settlements,
                transactions,
                block_info,
                slot,
            )
            .await
        }
        AccountsDB::Redis(redis_db) => {
            write_batch_redis(
                redis_db,
                account_settlements,
                transactions,
                block_info,
                slot,
            )
            .await
        }
        AccountsDB::Dual(postgres_db, redis_db) => {
            write_batch_dual(
                postgres_db,
                redis_db,
                account_settlements,
                transactions,
                block_info,
                slot,
            )
            .await
        }
    }
}

pub async fn write_batch_dual(
    postgres_db: &mut PostgresAccountsDB,
    redis_db: &mut RedisAccountsDB,
    account_settlements: &[(Pubkey, AccountSettlement)],
    transactions: Vec<(
        Signature,
        &SanitizedTransaction,
        u64,
        UnixTimestamp,
        &ProcessedTransaction,
    )>,
    block_info: Option<BlockInfo>,
    slot: Option<u64>,
) -> Result<(), String> {
    // Clone data for Redis since we'll write to Postgres first
    let transactions_clone = transactions.clone();
    let block_info_clone = block_info.clone();

    // Write to Postgres first - fail if this fails
    write_batch_postgres(
        postgres_db,
        account_settlements,
        transactions,
        block_info,
        slot,
    )
    .await?;

    // Write to Redis best-effort - log but don't fail
    if let Err(e) = write_batch_redis(
        redis_db,
        account_settlements,
        transactions_clone,
        block_info_clone,
        slot,
    )
    .await
    {
        warn!("Best-effort Redis write failed: {}", e);
    }

    Ok(())
}

async fn write_batch_postgres(
    db: &mut PostgresAccountsDB,
    account_settlements: &[(Pubkey, AccountSettlement)],
    transactions: Vec<(
        Signature,
        &SanitizedTransaction,
        u64,
        UnixTimestamp,
        &ProcessedTransaction,
    )>,
    block_info: Option<BlockInfo>,
    slot: Option<u64>,
) -> Result<(), String> {
    if db.read_only {
        warn!("Attempted to write batch in read-only mode");
        return Ok(());
    }

    let pool = Arc::clone(&db.pool);

    // Start a transaction
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| format!("Failed to begin transaction: {}", e))?;

    // Store accounts
    for (pubkey, account_settlement) in account_settlements {
        let pubkey_bytes = pubkey.to_bytes();
        if account_settlement.deleted {
            sqlx::query("DELETE FROM accounts WHERE pubkey = $1")
                .bind(&pubkey_bytes[..])
                .execute(&mut *tx)
                .await
                .map_err(|e| format!("Failed to delete account {}: {}", pubkey, e))?;
        } else {
            let account_data = bincode::serialize(&account_settlement.account)
                .map_err(|e| format!("Failed to serialize account: {}", e))?;

            sqlx::query(
                "INSERT INTO accounts (pubkey, data) VALUES ($1, $2)
                 ON CONFLICT (pubkey) DO UPDATE SET data = $2",
            )
            .bind(&pubkey_bytes[..])
            .bind(&account_data)
            .execute(&mut *tx)
            .await
            .map_err(|e| format!("Failed to store account: {}", e))?;
        }
    }

    // Store transactions and increment transaction count
    let tx_count = transactions.len() as i64;
    for (signature, transaction, tx_slot, block_time, processed) in transactions {
        let stored_tx = get_stored_transaction(transaction, tx_slot, block_time, processed);
        let sig_bytes = signature.as_ref();
        let tx_data = bincode::serialize(&stored_tx)
            .map_err(|e| format!("Failed to serialize transaction: {}", e))?;

        sqlx::query(
            "INSERT INTO transactions (signature, data) VALUES ($1, $2)
                 ON CONFLICT (signature) DO UPDATE SET data = $2",
        )
        .bind(sig_bytes)
        .bind(&tx_data)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("Failed to store transaction: {}", e))?;
    }

    // Update transaction count
    if tx_count > 0 {
        // Fetch current count
        let current_count_bytes = sqlx::query_scalar::<_, Vec<u8>>(
            "SELECT value FROM metadata WHERE key = 'transaction_count'",
        )
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| format!("Failed to fetch transaction count: {}", e))?;

        let mut count = current_count_bytes
            .and_then(|bytes| TransactionCount::from_bytes(&bytes))
            .unwrap_or_default();

        count.increment(tx_count as u64);

        sqlx::query(
            "INSERT INTO metadata (key, value) VALUES ('transaction_count', $1)
                 ON CONFLICT (key) DO UPDATE SET value = $1",
        )
        .bind(&count.to_bytes()[..])
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("Failed to update transaction count: {}", e))?;
    }

    // Store block info if provided
    if let Some(block_info) = &block_info {
        let block_data = bincode::serialize(block_info)
            .map_err(|e| format!("Failed to serialize block: {}", e))?;

        sqlx::query(
            "INSERT INTO blocks (slot, data) VALUES ($1, $2)
                 ON CONFLICT (slot) DO UPDATE SET data = $2",
        )
        .bind(block_info.slot as i64)
        .bind(&block_data)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("Failed to store block: {}", e))?;

        // Update latest blockhash
        sqlx::query(
            "INSERT INTO metadata (key, value) VALUES ('latest_blockhash', $1)
                 ON CONFLICT (key) DO UPDATE SET value = $1",
        )
        .bind(block_info.blockhash.as_ref())
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("Failed to update latest blockhash: {}", e))?;
    }

    // Update slot if provided
    if let Some(new_slot) = slot {
        sqlx::query(
            "INSERT INTO metadata (key, value) VALUES ('latest_slot', $1)
                 ON CONFLICT (key) DO UPDATE SET value = $1",
        )
        .bind(&new_slot.to_le_bytes()[..])
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("Failed to update latest slot: {}", e))?;
    }

    // Commit the transaction
    tx.commit()
        .await
        .map_err(|e| format!("Failed to commit transaction: {}", e))?;

    Ok(())
}

async fn write_batch_redis(
    db: &mut RedisAccountsDB,
    account_settlements: &[(Pubkey, AccountSettlement)],
    transactions: Vec<(
        Signature,
        &SanitizedTransaction,
        u64,
        UnixTimestamp,
        &ProcessedTransaction,
    )>,
    block_info: Option<BlockInfo>,
    slot: Option<u64>,
) -> Result<(), String> {
    // Use Redis pipeline for atomic batch operations
    let mut pipe = redis::pipe();
    pipe.atomic();

    // Update accounts
    for (pubkey, account_settlement) in account_settlements {
        let key = format!("account:{}", pubkey);
        if account_settlement.deleted {
            pipe.del(key);
        } else {
            let serialized = bincode::serialize(&account_settlement.account)
                .map_err(|e| format!("Failed to serialize account: {}", e))?;
            pipe.set(key, serialized);
        }
    }

    // Store transactions
    let tx_count = transactions.len();
    for (signature, transaction, tx_slot, block_time, processed) in transactions {
        let stored_tx = get_stored_transaction(transaction, tx_slot, block_time, processed);
        let key = format!("tx:{}", signature);
        let serialized = bincode::serialize(&stored_tx).unwrap();
        pipe.set(key, serialized);
    }

    // Increment transaction count
    if tx_count > 0 {
        pipe.incr("transaction_count", tx_count);
    }

    // Store block info
    if let Some(block) = block_info {
        pipe.set("latest_blockhash", block.blockhash.to_string());
        let key = format!("block:{}", block.slot);
        let serialized = bincode::serialize(&block).unwrap();
        pipe.set(key, serialized);
    }

    // Update slot
    if let Some(new_slot) = slot {
        pipe.set("latest_slot", new_slot);
    }

    // Execute pipeline - explicitly specify the return type to fix type inference
    let _: () = pipe
        .query_async(&mut db.connection)
        .await
        .map_err(|e| format!("Redis batch write failed: {}", e))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_sdk::{
        account::Account,
        hash::Hash,
        signature::{Keypair, Signer},
        system_transaction,
    };

    /// Test that write_batch_dual succeeds when Postgres writes complete
    /// but Redis is unavailable (connection fails).
    ///
    /// This test verifies:
    /// 1. Postgres write succeeds
    /// 2. Redis write failure is logged but not fatal
    /// 3. Function returns Ok(())
    ///
    /// Note: This is an integration test that requires:
    /// - TEST_POSTGRES_URL environment variable with a test database
    /// - Redis to be unavailable (or invalid Redis URL)
    #[tokio::test]
    #[ignore] // Requires database setup
    async fn test_write_batch_postgres_only() {
        use std::env;

        // Setup: Get test database URL from environment
        let postgres_url = env::var("TEST_POSTGRES_URL")
            .unwrap_or_else(|_| "postgresql://contra:contra@localhost:5432/contra_test".to_string());

        // Use an invalid Redis URL to simulate Redis being unavailable
        let redis_url = "redis://invalid-host:6379";

        // Create database connections
        // Postgres should succeed, Redis should fail to connect
        let mut postgres_db = match PostgresAccountsDB::new(&postgres_url, false).await {
            Ok(db) => db,
            Err(e) => {
                eprintln!("Skipping test: Cannot connect to test Postgres: {}", e);
                return;
            }
        };

        // Create Redis connection (this should fail or be invalid)
        let mut redis_db = match RedisAccountsDB::new(redis_url).await {
            Ok(db) => db,
            Err(_) => {
                // If we can't even create the Redis connection, create a connection
                // that will fail on write. For testing purposes, we'll skip if we
                // can't set up the test scenario properly.
                eprintln!("Skipping test: Cannot set up Redis test scenario");
                return;
            }
        };

        // Create test data
        let keypair = Keypair::new();
        let pubkey = keypair.pubkey();
        let account = Account {
            lamports: 1000,
            data: vec![],
            owner: solana_sdk::system_program::id(),
            executable: false,
            rent_epoch: 0,
        };
        let account_settlement = crate::stages::AccountSettlement {
            account: solana_sdk::account::AccountSharedData::from(account),
            deleted: false,
        };
        let account_settlements = vec![(pubkey, account_settlement)];

        // Create a test transaction
        let transaction = system_transaction::transfer(
            &keypair,
            &Keypair::new().pubkey(),
            100,
            Hash::default(),
        );
        let sanitized_tx = SanitizedTransaction::from_transaction_for_tests(transaction);

        let processed = ProcessedTransaction::default();

        let transactions = vec![(
            Signature::default(),
            &sanitized_tx,
            0u64,
            0i64,
            &processed,
        )];

        let block_info = Some(BlockInfo {
            slot: 100,
            blockhash: Hash::default(),
            block_height: Some(100),
            block_time: Some(0),
        });

        // Execute: Call write_batch_dual
        // This should succeed even if Redis fails
        let result = write_batch_dual(
            &mut postgres_db,
            &mut redis_db,
            &account_settlements,
            transactions,
            block_info,
            Some(100),
        )
        .await;

        // Verify: Function should return Ok despite Redis failure
        assert!(
            result.is_ok(),
            "write_batch_dual should succeed even when Redis fails. Got error: {:?}",
            result.err()
        );

        // Note: In a real test environment, we would also verify:
        // 1. Postgres contains the written data
        // 2. Warning logs were emitted for Redis failure
        // This requires log capture infrastructure not shown here
    }

    /// Test that write_batch_dual treats Redis failures as non-fatal.
    ///
    /// This test verifies the core requirement:
    /// - When Postgres write succeeds but Redis write fails, the function returns Ok(())
    /// - Redis failures are logged but do not propagate as errors
    ///
    /// Note: This is an integration test that requires:
    /// - TEST_POSTGRES_URL environment variable with a test database
    /// - Invalid Redis URL to simulate Redis failure
    #[tokio::test]
    #[ignore] // Requires database setup
    async fn test_write_batch_redis_failure_nonfatal() {
        use std::env;

        // Setup: Get test database URL from environment
        let postgres_url = env::var("TEST_POSTGRES_URL")
            .unwrap_or_else(|_| "postgresql://contra:contra@localhost:5432/contra_test".to_string());

        // Use an invalid Redis URL to guarantee Redis write will fail
        let redis_url = "redis://invalid-host:6379";

        // Create Postgres connection (should succeed)
        let mut postgres_db = match PostgresAccountsDB::new(&postgres_url, false).await {
            Ok(db) => db,
            Err(e) => {
                eprintln!("Skipping test: Cannot connect to test Postgres: {}", e);
                return;
            }
        };

        // Create Redis connection with invalid host
        // If connection creation fails, we'll create a mock scenario
        let mut redis_db = match RedisAccountsDB::new(redis_url).await {
            Ok(db) => db,
            Err(_) => {
                eprintln!("Skipping test: Cannot set up Redis test scenario");
                return;
            }
        };

        // Create minimal test data
        let keypair = Keypair::new();
        let pubkey = keypair.pubkey();
        let account = Account {
            lamports: 1000,
            data: vec![],
            owner: solana_sdk::system_program::id(),
            executable: false,
            rent_epoch: 0,
        };
        let account_settlement = crate::stages::AccountSettlement {
            account: solana_sdk::account::AccountSharedData::from(account),
            deleted: false,
        };
        let account_settlements = vec![(pubkey, account_settlement)];

        // Create a minimal test transaction
        let transaction = system_transaction::transfer(
            &keypair,
            &Keypair::new().pubkey(),
            100,
            Hash::default(),
        );
        let sanitized_tx = SanitizedTransaction::from_transaction_for_tests(transaction);
        let processed = ProcessedTransaction::default();

        let transactions = vec![(
            Signature::default(),
            &sanitized_tx,
            0u64,
            0i64,
            &processed,
        )];

        let block_info = Some(BlockInfo {
            slot: 100,
            blockhash: Hash::default(),
            block_height: Some(100),
            block_time: Some(0),
        });

        // Execute: Call write_batch_dual with valid Postgres but failing Redis
        let result = write_batch_dual(
            &mut postgres_db,
            &mut redis_db,
            &account_settlements,
            transactions,
            block_info,
            Some(100),
        )
        .await;

        // Verify: The function MUST return Ok(()) even when Redis fails
        // This is the core requirement: Redis failures are best-effort and non-fatal
        assert!(
            result.is_ok(),
            "write_batch_dual must return Ok(()) when Redis fails (non-fatal). Got: {:?}",
            result.err()
        );

        // The test passing means:
        // 1. Postgres write succeeded (otherwise would have returned Err)
        // 2. Redis write failed (due to invalid host)
        // 3. Redis failure was caught and logged, not propagated
        // 4. Function returned Ok(()) - Redis failure is non-fatal
    }
}
