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

    let slot = sqlx::query_scalar::<_, Option<i64>>("SELECT MIN(slot) FROM blocks")
        .fetch_one(pool.as_ref())
        .await
        .context("Failed to query first available block")?;

    slot.map(|s| s as u64)
        .context("No blocks found in database")
}

async fn get_first_available_block_redis(db: &RedisAccountsDB) -> Result<u64> {
    let mut conn = db.connection.clone();
    let result: redis::RedisResult<Option<u64>> = conn.get("first_available_block").await;
    result
        .map_err(|e| anyhow!("Failed to get first available block from Redis: {}", e))
        .and_then(|opt| opt.ok_or_else(|| anyhow!("No first available block found in Redis")))
}
