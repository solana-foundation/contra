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
    }
}

/// Writes a complete slot batch (accounts + transactions + block metadata) atomically.
/// Either every write in this batch commits, or none do — no partial slot state
/// is ever visible to readers.
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
    _slot: Option<u64>, // latest slot is derived from MAX(slot) in blocks table; no separate write needed
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
