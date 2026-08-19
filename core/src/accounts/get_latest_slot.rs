use {
    super::{postgres::PostgresAccountsDB, redis::RedisAccountsDB, traits::AccountsDB},
    anyhow::Result,
    tracing::warn,
};

pub async fn get_latest_slot(db: &AccountsDB) -> Result<Option<u64>> {
    match db {
        AccountsDB::Postgres(postgres_db) => get_latest_slot_postgres(postgres_db).await,
        AccountsDB::Redis(redis_db) => get_latest_slot_redis(redis_db).await,
    }
}

async fn get_latest_slot_postgres(db: &PostgresAccountsDB) -> Result<Option<u64>> {
    let pool = db.pool.clone();

    let result = sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(slot) FROM blocks")
        .fetch_one(pool.as_ref())
        .await;

    match result {
        Ok(slot) => Ok(slot.map(|s| s as u64)),
        Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("42P01") => {
            // "undefined_table" — schema not yet created, treat as fresh node
            Ok(None)
        }
        Err(e) => Err(anyhow::Error::from(e).context("Failed to query latest slot")),
    }
}

async fn get_latest_slot_redis(db: &RedisAccountsDB) -> Result<Option<u64>> {
    let cached = match db.get_trusted::<u64>("latest_slot").await {
        Ok(slot) => slot,
        Err(e) => {
            warn!("Failed to get latest slot from Redis: {}", e);
            None
        }
    };

    match cached {
        Some(slot) => Ok(Some(slot)),
        // No cached tip is a miss, not an empty ledger.
        None => get_latest_slot_postgres(&db.fallback).await,
    }
}
