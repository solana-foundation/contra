use {
    super::{
        postgres::PostgresAccountsDB,
        redis::RedisAccountsDB,
        traits::{AccountsDB, BlockInfo},
    },
    anyhow::{Context, Result},
    redis::AsyncCommands,
    sqlx::Row,
    std::sync::Arc,
    tracing::debug,
};

/// `Ok(None)` means the slot genuinely holds no block on this node, which is
/// routine because truncation prunes. Every storage or decode failure is an
/// `Err`, so a caller can never read an internal error as a skipped slot.
pub async fn get_block(db: &AccountsDB, slot: u64) -> Result<Option<BlockInfo>> {
    match db {
        AccountsDB::Postgres(postgres_db) => get_block_postgres(postgres_db, slot).await,
        AccountsDB::Redis(redis_db) => get_block_redis(redis_db, slot).await,
    }
}

async fn get_block_postgres(db: &PostgresAccountsDB, slot: u64) -> Result<Option<BlockInfo>> {
    let pool = Arc::clone(&db.pool);

    let row = sqlx::query("SELECT data FROM blocks WHERE slot = $1")
        .bind(slot as i64)
        .fetch_optional(pool.as_ref())
        .await
        .with_context(|| format!("Failed to read block at slot {}", slot))?;

    let Some(row) = row else {
        debug!("Block not found at slot {}", slot);
        return Ok(None);
    };

    let data: Vec<u8> = row.get("data");
    let block_info = bincode::deserialize(&data)
        .with_context(|| format!("Failed to deserialize block at slot {}", slot))?;
    debug!("Retrieved block at slot {}", slot);
    Ok(Some(block_info))
}

async fn get_block_redis(db: &RedisAccountsDB, slot: u64) -> Result<Option<BlockInfo>> {
    let mut conn = db.connection.clone();
    let key = format!("block:{}", slot);
    let data: Option<Vec<u8>> = conn
        .get(key)
        .await
        .with_context(|| format!("Failed to read block at slot {} from Redis", slot))?;

    let Some(bytes) = data else {
        return Ok(None);
    };
    let block_info = bincode::deserialize(&bytes)
        .with_context(|| format!("Failed to deserialize block at slot {}", slot))?;
    Ok(Some(block_info))
}
