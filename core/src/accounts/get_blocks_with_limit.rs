use {
    super::{postgres::PostgresAccountsDB, redis::RedisAccountsDB, traits::AccountsDB},
    anyhow::{anyhow, Context, Result},
};

/// Maximum number of blocks that can be returned (per Solana spec)
const MAX_BLOCKS_LIMIT: u64 = 500_000;

/// The first `limit` slots at or after `start_slot` that produced a block,
/// ascending. Unlike `get_blocks` the upper bound is a count, not a slot, so a
/// caller looking for the next producing slot does not have to guess how far
/// ahead to scan.
pub async fn get_blocks_with_limit(
    db: &AccountsDB,
    start_slot: u64,
    limit: u64,
) -> Result<Vec<u64>> {
    if limit > MAX_BLOCKS_LIMIT {
        return Err(anyhow!(
            "Limit too large: {} (max: {})",
            limit,
            MAX_BLOCKS_LIMIT
        ));
    }
    if limit == 0 {
        return Ok(vec![]);
    }

    match db {
        AccountsDB::Postgres(postgres_db) => {
            get_blocks_with_limit_postgres(postgres_db, start_slot, limit).await
        }
        AccountsDB::Redis(redis_db) => {
            get_blocks_with_limit_redis(redis_db, start_slot, limit).await
        }
    }
}

async fn get_blocks_with_limit_postgres(
    db: &PostgresAccountsDB,
    start_slot: u64,
    limit: u64,
) -> Result<Vec<u64>> {
    let pool = db.pool.clone();

    let slots = sqlx::query_scalar::<_, i64>(
        "SELECT slot FROM blocks WHERE slot >= $1 ORDER BY slot ASC LIMIT $2",
    )
    .bind(start_slot as i64)
    .bind(limit as i64)
    .fetch_all(pool.as_ref())
    .await
    .context("Failed to query blocks with limit")?;

    Ok(slots.into_iter().map(|s| s as u64).collect())
}

async fn get_blocks_with_limit_redis(
    db: &RedisAccountsDB,
    start_slot: u64,
    limit: u64,
) -> Result<Vec<u64>> {
    let mut conn = db.connection.clone();

    // ZRANGE ... BYSCORE returns ascending score (slot) order, and LIMIT stops the
    // scan at `limit` members rather than walking to the end. Requires Redis 6.2+.
    let slots: Vec<u64> = redis::cmd("ZRANGE")
        .arg("block_slot_index")
        .arg(start_slot)
        .arg("+inf")
        .arg("BYSCORE")
        .arg("LIMIT")
        .arg(0i64)
        .arg(limit as i64)
        .query_async(&mut conn)
        .await
        .map_err(|e| anyhow!("Failed to query block slot index in Redis: {}", e))?;

    Ok(slots)
}
