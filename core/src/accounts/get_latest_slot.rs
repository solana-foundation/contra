use {
    super::{postgres::PostgresAccountsDB, redis::RedisAccountsDB, traits::AccountsDB},
    anyhow::{anyhow, Context, Result},
    redis::AsyncCommands,
};

pub async fn get_latest_slot(db: &AccountsDB) -> Result<Option<u64>> {
    match db {
        AccountsDB::Postgres(postgres_db) => get_latest_slot_postgres(postgres_db).await,
        AccountsDB::Redis(redis_db) => get_latest_slot_redis(redis_db).await,
    }
}

async fn get_latest_slot_postgres(db: &PostgresAccountsDB) -> Result<Option<u64>> {
    let pool = db.pool.clone();

    let slot = sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(slot) FROM blocks")
        .fetch_one(pool.as_ref())
        .await
        .context("Failed to query latest slot")?;

    Ok(slot.map(|s| s as u64))
}

async fn get_latest_slot_redis(db: &RedisAccountsDB) -> Result<Option<u64>> {
    let mut conn = db.connection.clone();
    let result: redis::RedisResult<Option<u64>> = conn.get("latest_slot").await;
    result.map_err(|e| anyhow!("Failed to get latest slot from Redis: {}", e))
}
