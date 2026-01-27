use {
    super::{postgres::PostgresAccountsDB, redis::RedisAccountsDB, traits::AccountsDB},
    crate::accounts::types::StoredTransaction,
    redis::AsyncCommands,
    solana_sdk::signature::Signature,
    sqlx::Row,
    std::sync::Arc,
    tracing::{debug, error},
};

pub async fn get_transaction(db: &AccountsDB, signature: &Signature) -> Option<StoredTransaction> {
    match db {
        AccountsDB::Postgres(postgres_db) => get_transaction_postgres(postgres_db, signature).await,
        AccountsDB::Redis(redis_db) => get_transaction_redis(redis_db, signature).await,
    }
}

async fn get_transaction_postgres(
    db: &PostgresAccountsDB,
    signature: &Signature,
) -> Option<StoredTransaction> {
    let pool = Arc::clone(&db.pool);
    let sig_bytes = signature.as_ref().to_vec();
    let sig_str = signature.to_string();

    match sqlx::query("SELECT data FROM transactions WHERE signature = $1")
        .bind(&sig_bytes)
        .fetch_optional(pool.as_ref())
        .await
    {
        Ok(Some(row)) => {
            let data: Vec<u8> = row.get("data");
            match bincode::deserialize(&data) {
                Ok(tx) => {
                    debug!("Retrieved transaction {}", sig_str);
                    Some(tx)
                }
                Err(e) => {
                    error!("Failed to deserialize transaction {}: {}", sig_str, e);
                    None
                }
            }
        }
        Ok(None) => {
            debug!("Transaction {} not found", sig_str);
            None
        }
        Err(e) => {
            error!("Failed to read transaction {}: {}", sig_str, e);
            None
        }
    }
}

async fn get_transaction_redis(
    db: &RedisAccountsDB,
    signature: &Signature,
) -> Option<StoredTransaction> {
    let mut conn = db.connection.clone();
    let key = format!("tx:{}", signature);
    let data: redis::RedisResult<Vec<u8>> = conn.get(key).await;
    match data {
        Ok(bytes) => bincode::deserialize(&bytes).ok(),
        Err(_) => None,
    }
}
