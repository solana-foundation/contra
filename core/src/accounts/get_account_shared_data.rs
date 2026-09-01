use {
    super::{postgres::PostgresAccountsDB, redis::RedisAccountsDB, traits::AccountsDB},
    redis::AsyncCommands,
    solana_sdk::{
        account::{AccountSharedData, ReadableAccount},
        pubkey::Pubkey,
    },
    sqlx::Row,
    std::sync::Arc,
    tracing::{debug, error},
};

pub async fn get_account_shared_data(
    db: &AccountsDB,
    pubkey: &Pubkey,
) -> Option<AccountSharedData> {
    let account = match db {
        AccountsDB::Postgres(postgres_db) => {
            get_account_shared_data_postgres(postgres_db, pubkey).await
        }
        AccountsDB::Redis(redis_db) => get_account_shared_data_redis(redis_db, pubkey).await,
    };
    // A stored row with no lamports describes an account that no longer exists.
    account.filter(|account| account.lamports() != 0)
}

async fn get_account_shared_data_postgres(
    db: &PostgresAccountsDB,
    pubkey: &Pubkey,
) -> Option<AccountSharedData> {
    // Query from database
    let pool = Arc::clone(&db.pool);
    let pubkey_bytes = pubkey.to_bytes();

    match sqlx::query("SELECT data FROM accounts WHERE pubkey = $1")
        .bind(&pubkey_bytes[..])
        .fetch_optional(pool.as_ref())
        .await
    {
        Ok(Some(row)) => {
            let data: Vec<u8> = row.get("data");
            match bincode::deserialize::<AccountSharedData>(&data) {
                Ok(account) => {
                    debug!(
                        "Retrieved account {} with {} lamports",
                        pubkey,
                        account.lamports()
                    );
                    Some(account)
                }
                Err(e) => {
                    error!("Failed to deserialize account {}: {}", pubkey, e);
                    None
                }
            }
        }
        Ok(None) => {
            debug!("Account {} not found", pubkey);
            None
        }
        Err(e) => {
            error!("Failed to read account {}: {}", pubkey, e);
            None
        }
    }
}

async fn get_account_shared_data_redis(
    db: &RedisAccountsDB,
    pubkey: &Pubkey,
) -> Option<AccountSharedData> {
    let key = format!("account:{}", pubkey);
    let cached = match db.get_trusted::<Vec<u8>>(&key).await {
        Ok(bytes) => bytes,
        Err(e) => {
            error!("Failed to get account {} from Redis: {}", pubkey, e);
            None
        }
    };

    if let Some(bytes) = cached {
        match bincode::deserialize(&bytes) {
            Ok(account) => return Some(account),
            Err(e) => {
                error!("Failed to deserialize cached account {}: {}", pubkey, e);
                // Evict it, or every later read pays both hops to reach the same
                // conclusion. Nothing else would ever remove it.
                let mut conn = db.connection.clone();
                if let Err(e) = conn.del::<_, ()>(&key).await {
                    error!("Failed to evict corrupt cached account {}: {}", pubkey, e);
                }
            }
        }
    }

    // Absent, unreadable or corrupt cache entries are misses, not proof the
    // account does not exist. Resolve them against the source of truth.
    get_account_shared_data_postgres(&db.fallback, pubkey).await
}
