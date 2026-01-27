use {
    super::{
        postgres::PostgresAccountsDB,
        redis::RedisAccountsDB,
        traits::{AccountsDB, BlockInfo},
    },
    redis::AsyncCommands,
    sqlx::Row,
    std::sync::Arc,
    tracing::{debug, error},
};

pub async fn get_block(db: &AccountsDB, slot: u64) -> Option<BlockInfo> {
    match db {
        AccountsDB::Postgres(postgres_db) => get_block_postgres(postgres_db, slot).await,
        AccountsDB::Redis(redis_db) => get_block_redis(redis_db, slot).await,
    }
}

async fn get_block_postgres(db: &PostgresAccountsDB, slot: u64) -> Option<BlockInfo> {
    let pool = Arc::clone(&db.pool);

    match sqlx::query("SELECT data FROM blocks WHERE slot = $1")
        .bind(slot as i64)
        .fetch_optional(pool.as_ref())
        .await
    {
        Ok(Some(row)) => {
            let data: Vec<u8> = row.get("data");
            match bincode::deserialize(&data) {
                Ok(block_info) => {
                    debug!("Retrieved block at slot {}", slot);
                    Some(block_info)
                }
                Err(e) => {
                    error!("Failed to deserialize block info: {}", e);
                    None
                }
            }
        }
        Ok(None) => {
            debug!("Block not found at slot {}", slot);
            None
        }
        Err(e) => {
            error!("Failed to read block: {}", e);
            None
        }
    }
}

async fn get_block_redis(db: &RedisAccountsDB, slot: u64) -> Option<BlockInfo> {
    let mut conn = db.connection.clone();
    let key = format!("block:{}", slot);
    let data: redis::RedisResult<Vec<u8>> = conn.get(key).await;
    match data {
        Ok(bytes) => bincode::deserialize(&bytes).ok(),
        Err(_) => None,
    }
}
