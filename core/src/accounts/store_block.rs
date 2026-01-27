use {
    super::{
        postgres::PostgresAccountsDB,
        redis::RedisAccountsDB,
        traits::{AccountsDB, BlockInfo},
    },
    redis::AsyncCommands,
    std::sync::Arc,
    tracing::{debug, warn},
};

pub async fn store_block(db: &mut AccountsDB, block_info: BlockInfo) -> Result<(), String> {
    match db {
        AccountsDB::Postgres(postgres_db) => store_block_postgres(postgres_db, block_info).await,
        AccountsDB::Redis(redis_db) => store_block_redis(redis_db, block_info).await,
    }
}

async fn store_block_postgres(
    db: &mut PostgresAccountsDB,
    block_info: BlockInfo,
) -> Result<(), String> {
    if db.read_only {
        warn!("Attempted to store block in read-only mode");
        return Ok(());
    }

    let pool = Arc::clone(&db.pool);
    let slot = block_info.slot;
    let blockhash = block_info.blockhash;
    let tx_count = block_info.transaction_signatures.len();

    let block_data = bincode::serialize(&block_info)
        .map_err(|e| format!("Failed to serialize block info: {}", e))?;

    // Store block
    sqlx::query(
        "INSERT INTO blocks (slot, data) VALUES ($1, $2)
         ON CONFLICT (slot) DO UPDATE SET data = $2",
    )
    .bind(slot as i64)
    .bind(&block_data)
    .execute(pool.as_ref())
    .await
    .map_err(|e| format!("Failed to store block: {}", e))?;

    // Update latest blockhash
    sqlx::query(
        "INSERT INTO metadata (key, value) VALUES ('latest_blockhash', $1)
         ON CONFLICT (key) DO UPDATE SET value = $1",
    )
    .bind(blockhash.as_ref())
    .execute(pool.as_ref())
    .await
    .map_err(|e| format!("Failed to store latest blockhash: {}", e))?;

    debug!(
        "Stored block at slot {} with {} transactions",
        slot, tx_count
    );
    Ok(())
}

async fn store_block_redis(db: &mut RedisAccountsDB, block_info: BlockInfo) -> Result<(), String> {
    // Store blockhash
    let _: redis::RedisResult<()> = db
        .connection
        .set("latest_blockhash", block_info.blockhash.to_string())
        .await;

    // Store block info
    let key = format!("block:{}", block_info.slot);
    let serialized = bincode::serialize(&block_info).unwrap();
    let _: redis::RedisResult<()> = db.connection.set(key, serialized).await;
    Ok(())
}
