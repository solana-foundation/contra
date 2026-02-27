use {
    super::{postgres::PostgresAccountsDB, redis::RedisAccountsDB, types::StoredTransaction},
    crate::stages::AccountSettlement,
    anyhow::Result,
    serde::{Deserialize, Serialize},
    solana_sdk::{
        account::AccountSharedData, clock::UnixTimestamp, hash::Hash, pubkey::Pubkey,
        signature::Signature, transaction::SanitizedTransaction,
    },
    solana_svm::transaction_processing_result::ProcessedTransaction,
};

/// Block metadata stored in the database
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockInfo {
    pub slot: u64,
    pub blockhash: Hash,
    pub previous_blockhash: Hash,
    pub parent_slot: u64,
    pub block_height: Option<u64>,
    pub block_time: Option<i64>,
    /// Transaction signatures in this block, in order
    pub transaction_signatures: Vec<Signature>,
    /// The recent_blockhash each transaction referenced, parallel to transaction_signatures.
    /// Used to rebuild the dedup cache on restart.
    pub transaction_recent_blockhashes: Vec<Hash>,
}

/// AccountsDB enum supporting multiple backend storage options
///
/// # Variants
///
/// * `Postgres` - PostgreSQL database only. Provides ACID transactions and is the
///   source of truth for all finalized state.
///
/// * `Redis` - Redis cache only. Fast in-memory storage but lacks true transaction
///   support. Uses MULTI/EXEC which can fail partway through without rollback.
#[derive(Clone)]
#[allow(clippy::large_enum_variant)]
pub enum AccountsDB {
    Postgres(PostgresAccountsDB),
    Redis(RedisAccountsDB),
}

impl AccountsDB {
    pub async fn get_account_shared_data(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
        super::get_account_shared_data::get_account_shared_data(self, pubkey).await
    }

    pub async fn set_account(&mut self, pubkey: Pubkey, account: AccountSharedData) {
        super::set_account::set_account(self, pubkey, account).await
    }

    pub async fn get_transaction(&self, signature: &Signature) -> Option<StoredTransaction> {
        super::get_transaction::get_transaction(self, signature).await
    }

    pub async fn get_latest_slot(&self) -> Result<Option<u64>> {
        super::get_latest_slot::get_latest_slot(self).await
    }

    pub async fn set_latest_slot(&mut self, slot: u64) -> Result<(), String> {
        super::set_latest_slot::set_latest_slot(self, slot).await
    }

    pub async fn store_block(&mut self, block_info: BlockInfo) -> Result<(), String> {
        super::store_block::store_block(self, block_info).await
    }

    pub async fn get_block(&self, slot: u64) -> Option<BlockInfo> {
        super::get_block::get_block(self, slot).await
    }

    pub async fn get_latest_blockhash(&self) -> Result<Hash> {
        super::get_latest_blockhash::get_latest_blockhash(self).await
    }

    pub async fn get_transaction_count(&self) -> Result<u64> {
        super::get_transaction_count::get_transaction_count(self).await
    }

    pub async fn get_first_available_block(&self) -> Result<u64> {
        super::get_first_available_block::get_first_available_block(self).await
    }

    pub async fn get_blocks(&self, start_slot: u64, end_slot: Option<u64>) -> Result<Vec<u64>> {
        super::get_blocks::get_blocks(self, start_slot, end_slot).await
    }

    pub async fn get_blocks_in_range(
        &self,
        start_slot: u64,
        end_slot: u64,
    ) -> Result<Vec<BlockInfo>> {
        super::get_blocks_in_range::get_blocks_in_range(self, start_slot, end_slot).await
    }

    pub async fn get_epoch_info(&self) -> Result<crate::rpc::api::EpochInfo> {
        super::get_epoch_info::get_epoch_info(self).await
    }

    pub async fn write_batch(
        &mut self,
        account_settlements: &[(Pubkey, AccountSettlement)],
        transactions: Vec<(
            Signature,
            &SanitizedTransaction,
            u64, // slot
            UnixTimestamp,
            &ProcessedTransaction,
        )>,
        block_info: Option<BlockInfo>,
    ) -> Result<(), String> {
        super::write_batch::write_batch(self, account_settlements, transactions, block_info).await
    }

    pub async fn get_accounts(&self, accounts: &[Pubkey]) -> Vec<Option<AccountSharedData>> {
        super::get_accounts::get_accounts(self, accounts).await
    }

    pub async fn store_performance_sample(
        &mut self,
        sample: solana_rpc_client_types::response::RpcPerfSample,
    ) -> Result<()> {
        super::store_performance_sample::store_performance_sample(self, sample).await
    }

    pub async fn get_recent_performance_samples(
        &self,
        limit: usize,
    ) -> Result<Vec<solana_rpc_client_types::response::RpcPerfSample>> {
        super::get_recent_performance_samples::get_recent_performance_samples(self, limit).await
    }

    pub async fn get_block_time(&self, slot: u64) -> Option<i64> {
        super::get_block_time::get_block_time(self, slot).await
    }
}

impl AccountsDB {
    pub async fn new(accountsdb_connection_url: &str, read_only: bool) -> Result<Self> {
        if accountsdb_connection_url.starts_with("postgresql://")
            || accountsdb_connection_url.starts_with("postgres://")
        {
            Ok(AccountsDB::Postgres(
                PostgresAccountsDB::new(accountsdb_connection_url, read_only)
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to create PostgresAccountsDB: {}", e))?,
            ))
        } else if accountsdb_connection_url.starts_with("redis://") {
            Ok(AccountsDB::Redis(
                RedisAccountsDB::new(accountsdb_connection_url)
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to create RedisAccountsDB: {}", e))?,
            ))
        } else {
            Err(anyhow::anyhow!(
                "Unsupported accountsdb connection URL scheme: {}",
                accountsdb_connection_url
            ))
        }
    }
}
