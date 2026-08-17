use {
    super::{postgres::PostgresAccountsDB, redis::RedisAccountsDB, traits::AccountsDB},
    crate::accounts::types::StoredTransaction,
    anyhow::{Context, Result},
    redis::AsyncCommands,
    solana_sdk::signature::Signature,
    sqlx::Row,
    std::sync::Arc,
    tracing::debug,
};

/// `Ok(None)` means the signature is genuinely absent from this node's history.
/// Every storage or decode failure is an `Err`, so callers can never mistake an
/// internal error for proof that a transaction did not land.
pub async fn get_transaction(
    db: &AccountsDB,
    signature: &Signature,
) -> Result<Option<StoredTransaction>> {
    match db {
        AccountsDB::Postgres(postgres_db) => get_transaction_postgres(postgres_db, signature).await,
        AccountsDB::Redis(redis_db) => get_transaction_redis(redis_db, signature).await,
    }
}

async fn get_transaction_postgres(
    db: &PostgresAccountsDB,
    signature: &Signature,
) -> Result<Option<StoredTransaction>> {
    let pool = Arc::clone(&db.pool);
    let sig_bytes = signature.as_ref().to_vec();
    let sig_str = signature.to_string();

    let row = sqlx::query("SELECT data FROM transactions WHERE signature = $1")
        .bind(&sig_bytes)
        .fetch_optional(pool.as_ref())
        .await
        .with_context(|| format!("Failed to read transaction {}", sig_str))?;

    let Some(row) = row else {
        debug!("Transaction {} not found", sig_str);
        return Ok(None);
    };

    let data: Vec<u8> = row.get("data");
    let tx = bincode::deserialize(&data)
        .with_context(|| format!("Failed to deserialize transaction {}", sig_str))?;
    debug!("Retrieved transaction {}", sig_str);
    Ok(Some(tx))
}

async fn get_transaction_redis(
    db: &RedisAccountsDB,
    signature: &Signature,
) -> Result<Option<StoredTransaction>> {
    let mut conn = db.connection.clone();
    let key = format!("tx:{}", signature);
    let data: Option<Vec<u8>> = conn
        .get(key)
        .await
        .with_context(|| format!("Failed to read transaction {} from Redis", signature))?;

    let Some(bytes) = data else {
        return Ok(None);
    };
    let tx = bincode::deserialize(&bytes)
        .with_context(|| format!("Failed to deserialize transaction {}", signature))?;
    Ok(Some(tx))
}
