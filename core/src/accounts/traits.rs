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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{create_test_block_info, start_test_postgres};
    use solana_sdk::account::AccountSharedData;

    #[tokio::test(flavor = "multi_thread")]
    async fn unsupported_url_scheme_rejected() {
        let result = AccountsDB::new("ftp://localhost/db", false).await;
        assert!(result.is_err());
        let msg = format!("{}", result.err().unwrap());
        assert!(
            msg.contains("Unsupported"),
            "expected unsupported scheme error, got: {msg}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_and_get_account_round_trip() {
        let (mut db, _pg) = start_test_postgres().await;

        let pubkey = Pubkey::new_unique();
        let owner = Pubkey::new_unique();
        let account = AccountSharedData::new(42_000, 0, &owner);

        // miss before set
        assert!(db.get_account_shared_data(&pubkey).await.is_none());

        db.set_account(pubkey, account.clone()).await;

        let loaded = db.get_account_shared_data(&pubkey).await;
        assert!(loaded.is_some());
        assert_eq!(
            solana_sdk::account::ReadableAccount::lamports(&loaded.unwrap()),
            42_000
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_accounts_batch_partial_hit() {
        let (mut db, _pg) = start_test_postgres().await;

        let pk1 = Pubkey::new_unique();
        let pk2 = Pubkey::new_unique();
        let pk3 = Pubkey::new_unique();
        let acct = AccountSharedData::new(1, 0, &Pubkey::new_unique());

        db.set_account(pk2, acct.clone()).await;

        let results = db.get_accounts(&[pk1, pk2, pk3]).await;
        assert_eq!(results.len(), 3);
        assert!(results[0].is_none(), "pk1 was never stored");
        assert!(results[1].is_some(), "pk2 should be found");
        assert!(results[2].is_none(), "pk3 was never stored");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn store_block_and_get_block_round_trip() {
        let (mut db, _pg) = start_test_postgres().await;

        let blockhash = Hash::new_unique();
        let block = create_test_block_info(10, blockhash);

        db.store_block(block.clone()).await.unwrap();

        let loaded = db.get_block(10).await;
        assert!(loaded.is_some());
        let loaded = loaded.unwrap();
        assert_eq!(loaded.slot, 10);
        assert_eq!(loaded.blockhash, blockhash);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_block_miss_returns_none() {
        let (db, _pg) = start_test_postgres().await;
        assert!(db.get_block(999).await.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_latest_slot_empty_then_populated() {
        let (mut db, _pg) = start_test_postgres().await;

        // empty DB → None
        let slot = db.get_latest_slot().await.unwrap();
        assert_eq!(slot, None);

        // store a block
        db.store_block(create_test_block_info(5, Hash::new_unique()))
            .await
            .unwrap();

        let slot = db.get_latest_slot().await.unwrap();
        assert_eq!(slot, Some(5));

        // store higher block
        db.store_block(create_test_block_info(12, Hash::new_unique()))
            .await
            .unwrap();
        let slot = db.get_latest_slot().await.unwrap();
        assert_eq!(slot, Some(12));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_latest_blockhash_after_store() {
        let (mut db, _pg) = start_test_postgres().await;

        // no blockhash stored yet → error
        let err = db.get_latest_blockhash().await;
        assert!(err.is_err());

        let bh = Hash::new_unique();
        db.store_block(create_test_block_info(1, bh)).await.unwrap();

        let loaded = db.get_latest_blockhash().await.unwrap();
        assert_eq!(loaded, bh);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_blocks_returns_slot_numbers_in_order() {
        let (mut db, _pg) = start_test_postgres().await;

        for slot in [3, 7, 1, 10] {
            db.store_block(create_test_block_info(slot, Hash::new_unique()))
                .await
                .unwrap();
        }

        let slots = db.get_blocks(0, Some(20)).await.unwrap();
        assert_eq!(slots, vec![1, 3, 7, 10]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_blocks_in_range_filters_correctly() {
        let (mut db, _pg) = start_test_postgres().await;

        for slot in [5, 10, 15, 20] {
            db.store_block(create_test_block_info(slot, Hash::new_unique()))
                .await
                .unwrap();
        }

        let blocks = db.get_blocks_in_range(8, 18).await.unwrap();
        let slots: Vec<u64> = blocks.iter().map(|b| b.slot).collect();
        assert_eq!(slots, vec![10, 15]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_blocks_in_range_empty_range() {
        let (db, _pg) = start_test_postgres().await;
        let blocks = db.get_blocks_in_range(100, 200).await.unwrap();
        assert!(blocks.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_transaction_count_starts_at_zero() {
        let (db, _pg) = start_test_postgres().await;
        let count = db.get_transaction_count().await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_transaction_miss() {
        let (db, _pg) = start_test_postgres().await;
        let sig = Signature::new_unique();
        assert!(db.get_transaction(&sig).await.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_epoch_info_after_storing_blocks() {
        let (mut db, _pg) = start_test_postgres().await;

        db.store_block(create_test_block_info(42, Hash::new_unique()))
            .await
            .unwrap();

        let info = db.get_epoch_info().await.unwrap();
        assert_eq!(info.absolute_slot, 42);
        assert_eq!(info.block_height, 42);
        assert_eq!(info.epoch, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_first_available_block_after_storing() {
        let (mut db, _pg) = start_test_postgres().await;

        for slot in [10, 5, 20] {
            db.store_block(create_test_block_info(slot, Hash::new_unique()))
                .await
                .unwrap();
        }

        let first = db.get_first_available_block().await.unwrap();
        assert_eq!(first, 5);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn store_and_get_performance_sample() {
        let (mut db, _pg) = start_test_postgres().await;

        let sample = solana_rpc_client_types::response::RpcPerfSample {
            slot: 100,
            num_transactions: 500,
            num_slots: 10,
            sample_period_secs: 60,
            num_non_vote_transactions: Some(480),
        };

        db.store_performance_sample(sample.clone()).await.unwrap();

        let loaded = db.get_recent_performance_samples(10).await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].slot, 100);
        assert_eq!(loaded[0].num_transactions, 500);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_recent_performance_samples_empty() {
        let (db, _pg) = start_test_postgres().await;
        let loaded = db.get_recent_performance_samples(10).await.unwrap();
        assert!(loaded.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_block_time_returns_stored_time() {
        let (mut db, _pg) = start_test_postgres().await;

        let block = create_test_block_info(7, Hash::new_unique());
        let expected_time = block.block_time;
        db.store_block(block).await.unwrap();

        let time = db.get_block_time(7).await;
        assert_eq!(time, expected_time);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_batch_stores_accounts_and_block() {
        use crate::stages::AccountSettlement;

        let (mut db, _pg) = start_test_postgres().await;

        let pk = Pubkey::new_unique();
        let acct = AccountSharedData::new(1_000, 0, &Pubkey::new_unique());
        let settlement = AccountSettlement {
            account: acct.clone(),
            deleted: false,
        };

        let bh = Hash::new_unique();
        let block = create_test_block_info(1, bh);

        db.write_batch(&[(pk, settlement)], vec![], Some(block))
            .await
            .unwrap();

        // account was stored
        assert!(db.get_account_shared_data(&pk).await.is_some());

        // block was stored
        let loaded = db.get_block(1).await;
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().blockhash, bh);

        // latest blockhash was updated
        assert_eq!(db.get_latest_blockhash().await.unwrap(), bh);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_batch_deleted_account_removes_from_db() {
        use crate::stages::AccountSettlement;

        let (mut db, _pg) = start_test_postgres().await;

        let pk = Pubkey::new_unique();
        let acct = AccountSharedData::new(500, 0, &Pubkey::new_unique());

        // first store an account
        db.set_account(pk, acct.clone()).await;
        assert!(db.get_account_shared_data(&pk).await.is_some());

        // now write_batch with deleted=true
        let settlement = AccountSettlement {
            account: acct,
            deleted: true,
        };
        db.write_batch(&[(pk, settlement)], vec![], None)
            .await
            .unwrap();

        // account is gone
        assert!(db.get_account_shared_data(&pk).await.is_none());
    }
}
