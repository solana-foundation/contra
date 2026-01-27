use {
    anyhow::Result,
    redis::{aio::ConnectionManager, AsyncCommands, RedisResult},
    solana_sdk::{account::AccountSharedData, pubkey::Pubkey},
    solana_svm_callback::{InvokeContextCallback, TransactionProcessingCallback},
};

#[derive(Clone)]
pub struct RedisAccountsDB {
    pub connection: ConnectionManager,
}

impl RedisAccountsDB {
    pub async fn new(redis_url: &str) -> Result<Self, String> {
        let client = redis::Client::open(redis_url)
            .map_err(|e| format!("Failed to create Redis client: {}", e))?;
        let connection = ConnectionManager::new(client)
            .await
            .map_err(|e| format!("Failed to connect to Redis: {}", e))?;

        let db = Self { connection };
        Ok(db)
    }

    pub async fn set_account(&mut self, pubkey: Pubkey, account: AccountSharedData) {
        let key = format!("account:{}", pubkey);
        let serialized = bincode::serialize(&account).unwrap();
        let _: RedisResult<()> = self.connection.set(key, serialized).await;
    }
}

impl InvokeContextCallback for RedisAccountsDB {}

impl TransactionProcessingCallback for RedisAccountsDB {
    fn get_account_shared_data(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
        let db = super::traits::AccountsDB::Redis(self.clone());
        let pubkey = *pubkey;
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                super::get_account_shared_data::get_account_shared_data(&db, &pubkey).await
            })
        })
    }

    fn account_matches_owners(&self, account: &Pubkey, owners: &[Pubkey]) -> Option<usize> {
        let db = super::traits::AccountsDB::Redis(self.clone());
        let account = *account;
        let owners = owners.to_vec();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                super::account_matches_owners::account_matches_owners(&db, &account, &owners).await
            })
        })
    }
}
