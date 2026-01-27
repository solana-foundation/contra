use {
    super::{postgres::PostgresAccountsDB, redis::RedisAccountsDB, traits::AccountsDB},
    anyhow::{Context, Result},
    redis::AsyncCommands,
    solana_rpc_client_types::response::RpcPerfSample,
};

pub async fn store_performance_sample(db: &mut AccountsDB, sample: RpcPerfSample) -> Result<()> {
    match db {
        AccountsDB::Postgres(postgres_db) => {
            store_performance_sample_postgres(postgres_db, sample).await
        }
        AccountsDB::Redis(redis_db) => store_performance_sample_redis(redis_db, sample).await,
    }
}

async fn store_performance_sample_postgres(
    db: &mut PostgresAccountsDB,
    sample: RpcPerfSample,
) -> Result<()> {
    let pool = db.pool.clone();

    sqlx::query(
        r#"
        INSERT INTO performance_samples (slot, num_transactions, num_slots, sample_period_secs, num_non_vote_transactions)
        VALUES ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(sample.slot as i64)
    .bind(sample.num_transactions as i64)
    .bind(sample.num_slots as i64)
    .bind(sample.sample_period_secs as i16)
    .bind(sample.num_non_vote_transactions.unwrap_or(0) as i64)
    .execute(pool.as_ref())
    .await
    .context("Failed to store performance sample")?;

    Ok(())
}

async fn store_performance_sample_redis(
    db: &mut RedisAccountsDB,
    sample: RpcPerfSample,
) -> Result<()> {
    let mut conn = db.connection.clone();

    // Serialize the performance sample as JSON
    let sample_json =
        serde_json::to_string(&sample).context("Failed to serialize performance sample")?;

    // Store in a Redis list with a limited size (max 720 samples)
    // Use LPUSH to add to the front and LTRIM to keep only the most recent samples
    conn.lpush::<_, _, ()>("performance_samples", sample_json)
        .await
        .context("Failed to push performance sample to Redis")?;

    // Keep only the most recent 720 samples
    conn.ltrim::<_, ()>("performance_samples", 0, 719)
        .await
        .context("Failed to trim performance samples list")?;

    Ok(())
}
