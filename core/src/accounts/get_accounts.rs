use {
    super::traits::AccountsDB,
    crate::accounts::{PostgresAccountsDB, RedisAccountsDB},
    redis::{AsyncCommands, RedisResult},
    solana_sdk::{
        account::{AccountSharedData, ReadableAccount},
        pubkey::Pubkey,
    },
    sqlx::Row,
    std::sync::Arc,
};

pub async fn get_accounts(db: &AccountsDB, accounts: &[Pubkey]) -> Vec<Option<AccountSharedData>> {
    let mut results = match db {
        AccountsDB::Postgres(postgres_db) => get_accounts_postgres(postgres_db, accounts).await,
        AccountsDB::Redis(redis_db) => get_accounts_redis(redis_db, accounts).await,
    };
    // A stored row with no lamports describes an account that no longer exists.
    // Cleared in place so the result stays positionally aligned with `accounts`.
    for slot in results.iter_mut() {
        if slot.as_ref().is_some_and(|account| account.lamports() == 0) {
            *slot = None;
        }
    }
    results
}

async fn get_accounts_postgres(
    postgres_db: &PostgresAccountsDB,
    accounts: &[Pubkey],
) -> Vec<Option<AccountSharedData>> {
    let pool = Arc::clone(&postgres_db.pool);
    let pubkey_bytes: Vec<Vec<u8>> = accounts.iter().map(|key| key.to_bytes().to_vec()).collect();

    match sqlx::query("SELECT pubkey, data FROM accounts WHERE pubkey = ANY($1)")
        .bind(&pubkey_bytes)
        .fetch_all(pool.as_ref())
        .await
    {
        Ok(rows) => {
            // Initialize result vector with None for all accounts
            let mut result = vec![None; accounts.len()];

            for row in rows {
                let pubkey_bytes: Vec<u8> = row.get("pubkey");
                let data: Vec<u8> = row.get("data");

                // Find the index of this pubkey in the original input
                if let Some(index) = accounts
                    .iter()
                    .position(|&key| key.to_bytes().as_slice() == pubkey_bytes)
                {
                    match bincode::deserialize::<AccountSharedData>(&data) {
                        Ok(account) => result[index] = Some(account),
                        Err(e) => {
                            tracing::error!("Failed to deserialize account data: {}", e);
                        }
                    }
                }
            }
            result
        }
        Err(e) => {
            tracing::error!("Failed to fetch accounts: {}", e);
            vec![None; accounts.len()]
        }
    }
}

async fn get_accounts_redis(
    redis_db: &RedisAccountsDB,
    accounts: &[Pubkey],
) -> Vec<Option<AccountSharedData>> {
    // MGET with no keys is a Redis error, and there is nothing to resolve.
    if accounts.is_empty() {
        return Vec::new();
    }

    let mut conn = redis_db.connection.clone();
    // The deployment stamp rides along as the first key, so the same round trip
    // that fetches the values also proves the cache may still be read from.
    let mut keys = Vec::with_capacity(accounts.len() + 1);
    keys.push(crate::accounts::redis_coherence::DEPLOYMENT_ID_KEY.to_string());
    keys.extend(accounts.iter().map(|key| format!("account:{}", key)));
    let data: RedisResult<Vec<Option<Vec<u8>>>> = conn.mget(keys).await;

    let mut results: Vec<Option<AccountSharedData>> = match data {
        // MGET answers one entry per key, so the split always succeeds; a short
        // reply is treated as an untrusted cache rather than indexed into, since
        // this runs under an RPC request where a panic is far worse than a miss.
        Ok(cached) if cached.len() == accounts.len() + 1 => {
            let (stamp, values) = cached.split_first().expect("checked non-empty above");
            if redis_db.stamp_is_current(stamp.as_ref()) {
                values
                    .iter()
                    .map(|opt| {
                        opt.as_ref()
                            .and_then(|bytes| bincode::deserialize(bytes).ok())
                    })
                    .collect()
            } else {
                // Condemned or foreign cache: every position is a miss.
                vec![None; accounts.len()]
            }
        }
        Ok(cached) => {
            tracing::error!(
                "Redis returned {} values for {} keys; treating the cache as unusable",
                cached.len(),
                accounts.len() + 1
            );
            vec![None; accounts.len()]
        }
        Err(e) => {
            tracing::error!("Failed to read accounts from Redis: {}", e);
            vec![None; accounts.len()]
        }
    };

    // Every position the cache could not answer is a miss. Resolve just those
    // against the source of truth, keeping the result aligned with `accounts`.
    let missing: Vec<usize> = results
        .iter()
        .enumerate()
        .filter(|(_, account)| account.is_none())
        .map(|(position, _)| position)
        .collect();
    if missing.is_empty() {
        return results;
    }

    let missing_pubkeys: Vec<Pubkey> = missing.iter().map(|&position| accounts[position]).collect();
    let resolved = get_accounts_postgres(&redis_db.fallback, &missing_pubkeys).await;
    for (position, account) in missing.into_iter().zip(resolved) {
        results[position] = account;
    }
    results
}
