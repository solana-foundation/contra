use {
    super::{
        postgres::PostgresAccountsDB, redis::RedisAccountsDB, traits::AccountsDB,
        transaction_count::TransactionCount,
    },
    anyhow::{anyhow, Context, Result},
    redis::AsyncCommands,
};

pub async fn get_transaction_count(db: &AccountsDB) -> Result<u64> {
    match db {
        AccountsDB::Postgres(postgres_db) => get_transaction_count_postgres(postgres_db).await,
        AccountsDB::Redis(redis_db) => get_transaction_count_redis(redis_db).await,
    }
}

async fn get_transaction_count_postgres(db: &PostgresAccountsDB) -> Result<u64> {
    let pool = db.pool.clone();

    let count_bytes = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT value FROM metadata WHERE key = 'transaction_count'",
    )
    .fetch_optional(pool.as_ref())
    .await
    .context("Failed to query transaction count")?;

    let count = count_bytes
        .and_then(|bytes| TransactionCount::from_bytes(&bytes))
        .unwrap_or_default();

    Ok(count.count())
}

async fn get_transaction_count_redis(db: &RedisAccountsDB) -> Result<u64> {
    let mut conn = db.connection.clone();
    let result: redis::RedisResult<Option<u64>> = conn.get("transaction_count").await;
    result
        .map_err(|e| anyhow!("Failed to get transaction count from Redis: {}", e))
        .map(|opt| opt.unwrap_or(0))
}
