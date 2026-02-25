use {
    super::{
        get_account_shared_data::get_account_shared_data, postgres::PostgresAccountsDB,
        redis::RedisAccountsDB, traits::AccountsDB,
    },
    solana_sdk::{account::ReadableAccount, pubkey::Pubkey},
};

pub async fn account_matches_owners(
    db: &AccountsDB,
    account: &Pubkey,
    owners: &[Pubkey],
) -> Option<usize> {
    match db {
        AccountsDB::Postgres(postgres_db) => {
            account_matches_owners_postgres(postgres_db, account, owners).await
        }
        AccountsDB::Redis(redis_db) => {
            account_matches_owners_redis(redis_db, account, owners).await
        }
        AccountsDB::Dual(postgres_db, _redis_db) => {
            // For Dual mode, read from Postgres (source of truth)
            account_matches_owners_postgres(postgres_db, account, owners).await
        }
    }
}

async fn account_matches_owners_postgres(
    db: &PostgresAccountsDB,
    account: &Pubkey,
    owners: &[Pubkey],
) -> Option<usize> {
    let db = AccountsDB::Postgres(db.clone());
    let account_data = get_account_shared_data(&db, account).await;
    account_data.and_then(|account| owners.iter().position(|key| account.owner().eq(key)))
}

async fn account_matches_owners_redis(
    db: &RedisAccountsDB,
    account: &Pubkey,
    owners: &[Pubkey],
) -> Option<usize> {
    let db = AccountsDB::Redis(db.clone());
    let account_data = get_account_shared_data(&db, account).await;
    account_data.and_then(|account| owners.iter().position(|key| account.owner().eq(key)))
}
