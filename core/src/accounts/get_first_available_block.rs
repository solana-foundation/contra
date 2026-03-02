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
    if value.len() == 8 {
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(value);
        return Some(u64::from_le_bytes(bytes));
    }

    std::str::from_utf8(value)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
}

async fn get_first_available_block_redis(db: &RedisAccountsDB) -> Result<u64> {
    let mut conn = db.connection.clone();
    let result: redis::RedisResult<Option<u64>> = conn.get("first_available_block").await;
    result
        .map_err(|e| anyhow!("Failed to get first available block from Redis: {}", e))
        .and_then(|opt| opt.ok_or_else(|| anyhow!("No first available block found in Redis")))
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
    fn decode_first_available_block_supports_text_values() {
        assert_eq!(decode_first_available_block(b"12345"), Some(12345));
    }

    #[test]
    fn decode_first_available_block_rejects_invalid_values() {
        assert_eq!(decode_first_available_block(b"not-a-slot"), None);
    }
}
