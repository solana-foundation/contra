use {
    super::{
        postgres::PostgresAccountsDB,
        redis::RedisAccountsDB,
        traits::{AccountsDB, BlockInfo},
    },
    anyhow::{Context, Result},
    redis::AsyncCommands,
    sqlx::Row,
    tracing::{error, warn},
};

pub async fn get_blocks_in_range(
    db: &AccountsDB,
    start_slot: u64,
    end_slot: u64,
) -> Result<Vec<BlockInfo>> {
    match db {
        AccountsDB::Postgres(postgres_db) => {
            get_blocks_in_range_postgres(postgres_db, start_slot, end_slot).await
        }
        AccountsDB::Redis(redis_db) => {
            get_blocks_in_range_redis(redis_db, start_slot, end_slot).await
        }
    }
}

async fn get_blocks_in_range_postgres(
    db: &PostgresAccountsDB,
    start_slot: u64,
    end_slot: u64,
) -> Result<Vec<BlockInfo>> {
    let pool = db.pool.clone();

    let rows = sqlx::query(
        "SELECT data FROM blocks WHERE slot >= $1 AND slot <= $2 ORDER BY slot ASC",
    )
    .bind(start_slot as i64)
    .bind(end_slot as i64)
    .fetch_all(pool.as_ref())
    .await
    .context("Failed to query blocks in range")?;

    let mut blocks = Vec::with_capacity(rows.len());
    for row in rows {
        let data: Vec<u8> = row.get("data");
        match bincode::deserialize::<BlockInfo>(&data) {
            Ok(block) => blocks.push(block),
            Err(e) => error!("Failed to deserialize block in range query: {}", e),
        }
    }

    Ok(blocks)
}

async fn get_blocks_in_range_redis(
    db: &RedisAccountsDB,
    start_slot: u64,
    end_slot: u64,
) -> Result<Vec<BlockInfo>> {
    if start_slot > end_slot {
        return Ok(Vec::new());
    }

    let mut conn = db.connection.clone();

    // Build all keys for the slot range upfront, then MGET in one round-trip
    let keys: Vec<String> = (start_slot..=end_slot)
        .map(|slot| format!("block:{}", slot))
        .collect();

    let values: Vec<Option<Vec<u8>>> = conn
        .mget(&keys)
        .await
        .context("Failed to MGET blocks from Redis")?;

    let mut blocks = Vec::new();
    for (slot, maybe_bytes) in (start_slot..=end_slot).zip(values) {
        let Some(bytes) = maybe_bytes else {
            continue;
        };
        match bincode::deserialize::<BlockInfo>(&bytes) {
            Ok(block) => blocks.push(block),
            Err(e) => warn!("Failed to deserialize block {} from Redis: {}", slot, e),
        }
    }

    Ok(blocks)
}
