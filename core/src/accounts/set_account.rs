use {
    super::{postgres::PostgresAccountsDB, redis::RedisAccountsDB, traits::AccountsDB},
    redis::AsyncCommands,
    solana_sdk::{
        account::{AccountSharedData, ReadableAccount},
        pubkey::Pubkey,
    },
    std::sync::Arc,
    tracing::{debug, error, warn},
};

pub async fn set_account(db: &mut AccountsDB, pubkey: Pubkey, account: AccountSharedData) {
    match db {
        AccountsDB::Postgres(postgres_db) => {
            set_account_postgres(postgres_db, pubkey, account).await
        }
        AccountsDB::Redis(redis_db) => set_account_redis(redis_db, pubkey, account).await,
        // Dual backend: write to Postgres first, then Redis (best-effort)
        AccountsDB::Dual(postgres_db, redis_db) => {
            // Write to Postgres (blocking)
            set_account_postgres(postgres_db, pubkey, account.clone()).await;
            // Write to Redis (best-effort, non-fatal)
            if let Err(e) = bincode::serialize(&account) {
                warn!("Failed to serialize account for Redis cache: {}", e);
            } else {
                set_account_redis(redis_db, pubkey, account).await;
            }
        }
    }
}

async fn set_account_postgres(
    db: &mut PostgresAccountsDB,
    pubkey: Pubkey,
    account: AccountSharedData,
) {
    if db.read_only {
        warn!("Attempted to set account {} in read-only mode", pubkey);
        return;
    }

    let pool = Arc::clone(&db.pool);
    let pubkey_bytes = pubkey.to_bytes();
    let account_data = match bincode::serialize(&account) {
        Ok(data) => data,
        Err(e) => {
            error!("Failed to serialize account {}: {}", pubkey, e);
            return;
        }
    };

    if let Err(e) = sqlx::query(
        "INSERT INTO accounts (pubkey, data) VALUES ($1, $2)
         ON CONFLICT (pubkey) DO UPDATE SET data = $2",
    )
    .bind(&pubkey_bytes[..])
    .bind(&account_data)
    .execute(pool.as_ref())
    .await
    {
        error!("Failed to store account {}: {}", pubkey, e);
    } else {
        debug!(
            "Stored account {} with {} lamports",
            pubkey,
            account.lamports()
        );
    }
}

async fn set_account_redis(db: &mut RedisAccountsDB, pubkey: Pubkey, account: AccountSharedData) {
    let key = format!("account:{}", pubkey);
    let serialized = bincode::serialize(&account).unwrap();
    let _: redis::RedisResult<()> = db.connection.set(key, serialized).await;
}
