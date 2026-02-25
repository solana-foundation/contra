use {
    super::{postgres::PostgresAccountsDB, redis::RedisAccountsDB, traits::AccountsDB},
    anyhow::{Context, Result},
    redis::AsyncCommands,
    solana_rpc_client_types::response::RpcPerfSample,
};

pub async fn get_recent_performance_samples(
    db: &AccountsDB,
    limit: usize,
) -> Result<Vec<RpcPerfSample>> {
    match db {
        AccountsDB::Postgres(postgres_db) => {
            get_recent_performance_samples_postgres(postgres_db, limit).await
        }
        AccountsDB::Redis(redis_db) => get_recent_performance_samples_redis(redis_db, limit).await,
        // Dual backend: read from Postgres (source of truth), not Redis cache
        AccountsDB::Dual(postgres_db, _redis_db) => {
            get_recent_performance_samples_postgres(postgres_db, limit).await
        }
    }
}

async fn get_recent_performance_samples_postgres(
    db: &PostgresAccountsDB,
    limit: usize,
) -> Result<Vec<RpcPerfSample>> {
    let pool = db.pool.clone();

    let samples = sqlx::query_as::<_, (i64, i64, i64, i16, i64)>(
        r#"
        SELECT slot, num_transactions, num_slots, sample_period_secs, num_non_vote_transactions
        FROM performance_samples
        ORDER BY slot DESC
        LIMIT $1
        "#,
    )
    .bind(limit as i64)
    .fetch_all(pool.as_ref())
    .await
    .context("Failed to fetch performance samples")?;

    let performance_samples = samples
        .into_iter()
        .map(
            |(slot, num_transactions, num_slots, sample_period_secs, num_non_vote_transactions)| {
                RpcPerfSample {
                    slot: slot as u64,
                    num_transactions: num_transactions as u64,
                    num_slots: num_slots as u64,
                    sample_period_secs: sample_period_secs as u16,
                    num_non_vote_transactions: Some(num_non_vote_transactions as u64),
                }
            },
        )
        .collect();

    Ok(performance_samples)
}

async fn get_recent_performance_samples_redis(
    db: &RedisAccountsDB,
    limit: usize,
) -> Result<Vec<RpcPerfSample>> {
    let mut conn = db.connection.clone();

    // Get the most recent samples from the list (0 to limit-1)
    let sample_jsons: Vec<String> = conn
        .lrange("performance_samples", 0, (limit - 1) as isize)
        .await
        .context("Failed to get performance samples from Redis")?;

    let mut performance_samples = Vec::new();
    for sample_json in sample_jsons {
        if let Ok(sample) = serde_json::from_str::<RpcPerfSample>(&sample_json) {
            performance_samples.push(sample);
        }
    }

    Ok(performance_samples)
}
