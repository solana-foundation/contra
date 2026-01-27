use {
    super::{postgres::PostgresAccountsDB, redis::RedisAccountsDB, traits::AccountsDB},
    redis::AsyncCommands,
    tracing::warn,
};

pub async fn set_latest_slot(db: &mut AccountsDB, slot: u64) -> Result<(), String> {
    match db {
        AccountsDB::Postgres(postgres_db) => set_latest_slot_postgres(postgres_db, slot).await,
        AccountsDB::Redis(redis_db) => set_latest_slot_redis(redis_db, slot).await,
    }
}

async fn set_latest_slot_postgres(db: &mut PostgresAccountsDB, _slot: u64) -> Result<(), String> {
    if db.read_only {
        warn!("Attempted to set latest slot in read-only mode");
        return Ok(());
    }

    // For Postgres, latest slot is determined by the blocks table
    // No separate field needed as it's computed from MAX(slot)
    Ok(())
}

async fn set_latest_slot_redis(db: &mut RedisAccountsDB, slot: u64) -> Result<(), String> {
    let _: redis::RedisResult<()> = db.connection.set("latest_slot", slot).await;
    Ok(())
}
