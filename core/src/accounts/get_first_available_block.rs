use {
    super::{postgres::PostgresAccountsDB, redis::RedisAccountsDB, traits::AccountsDB},
    anyhow::{anyhow, Context, Result},
    redis::AsyncCommands,
};

pub async fn get_first_available_block(db: &AccountsDB) -> Result<u64> {
    match db {
        AccountsDB::Postgres(postgres_db) => get_first_available_block_postgres(postgres_db).await,
        AccountsDB::Redis(redis_db) => get_first_available_block_redis(redis_db).await,
    }
}

async fn get_first_available_block_postgres(db: &PostgresAccountsDB) -> Result<u64> {
    let pool = db.pool.clone();

    let metadata_slot = sqlx::query_scalar::<_, Option<Vec<u8>>>(
        "SELECT value FROM metadata WHERE key = 'first_available_block'",
    )
    .fetch_optional(pool.as_ref())
    .await
    .context("Failed to query first_available_block metadata")?
    .flatten()
    .and_then(|value| decode_first_available_block(&value));

    if let Some(slot) = metadata_slot {
        return Ok(slot);
    }

    let slot = sqlx::query_scalar::<_, Option<i64>>("SELECT MIN(slot) FROM blocks")
        .fetch_one(pool.as_ref())
        .await
        .context("Failed to query first available block")?;

    slot.map(|s| s as u64)
        .context("No blocks found in database")
}

fn decode_first_available_block(value: &[u8]) -> Option<u64> {
    let bytes: [u8; 8] = value.try_into().ok()?;
    Some(u64::from_le_bytes(bytes))
}

async fn get_first_available_block_redis(db: &RedisAccountsDB) -> Result<u64> {
    let mut conn = db.connection.clone();
    // ZRANGE 0 0 returns the single member with the lowest score (earliest slot).
    // Pairs with write_batch_redis which uses ZADD to maintain proper MIN semantics.
    let result: redis::RedisResult<Vec<u64>> =
        conn.zrange("first_available_block_zset", 0isize, 0isize).await;
    result
        .map_err(|e| anyhow!("Failed to get first available block from Redis: {}", e))
        .and_then(|slots| {
            slots
                .into_iter()
                .next()
                .ok_or_else(|| anyhow!("No first available block found in Redis"))
        })
}

#[cfg(test)]
mod tests {
    use super::decode_first_available_block;

    #[test]
    fn decode_first_available_block_supports_u64_le_bytes() {
        let encoded = 42_u64.to_le_bytes();
        assert_eq!(decode_first_available_block(&encoded), Some(42));
    }

    #[test]
    fn decode_first_available_block_rejects_wrong_length() {
        assert_eq!(decode_first_available_block(b"short"), None);
    }
}
