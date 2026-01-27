use {
    super::{postgres::PostgresAccountsDB, redis::RedisAccountsDB, traits::AccountsDB},
    anyhow::{anyhow, Context, Result},
    redis::AsyncCommands,
    solana_sdk::hash::Hash,
    std::str::FromStr,
};

pub async fn get_latest_blockhash(db: &AccountsDB) -> Result<Hash> {
    match db {
        AccountsDB::Postgres(postgres_db) => get_latest_blockhash_postgres(postgres_db).await,
        AccountsDB::Redis(redis_db) => get_latest_blockhash_redis(redis_db).await,
    }
}

async fn get_latest_blockhash_postgres(db: &PostgresAccountsDB) -> Result<Hash> {
    let pool = db.pool.clone();
    // Get the latest blockhash from metadata table
    let blockhash_bytes: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT value FROM metadata WHERE key = 'latest_blockhash'")
            .fetch_optional(pool.as_ref())
            .await
            .context("Failed to query latest blockhash")?;

    if let Some(bytes) = blockhash_bytes {
        // The blockhash is stored as raw bytes (32 bytes)
        let hash_array: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("Invalid blockhash bytes length: {}", bytes.len()))?;
        Ok(Hash::new_from_array(hash_array))
    } else {
        Err(anyhow!("No blockhash found in metadata table"))
    }
}

async fn get_latest_blockhash_redis(db: &RedisAccountsDB) -> Result<Hash> {
    let mut conn = db.connection.clone();
    let result: redis::RedisResult<Option<String>> = conn.get("latest_blockhash").await;
    result
        .map_err(|e| anyhow!("Failed to get latest blockhash from Redis: {}", e))
        .and_then(|opt| {
            opt.ok_or_else(|| anyhow!("No latest blockhash found in Redis"))
                .and_then(|hash_str| {
                    Hash::from_str(&hash_str)
                        .map_err(|e| anyhow!("Invalid blockhash format: {}", e))
                })
        })
}
