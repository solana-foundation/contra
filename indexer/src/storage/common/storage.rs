pub use super::models::*;

pub mod close;
pub mod count_pending_transactions;
pub mod drop_tables;
pub mod get_all_db_transactions;
pub mod get_and_lock_pending_transactions;
pub mod get_committed_checkpoint;
pub mod get_completed_withdrawal_nonces;
pub mod get_escrow_balances_by_mint;
pub mod get_mint;
pub mod get_mint_balances_for_reconciliation;
pub mod get_pending_db_transactions;
pub mod init_schema;
pub mod insert_db_transaction;
pub mod insert_db_transactions_batch;
pub mod update_committed_checkpoint;
pub mod update_transaction_status;
pub mod upsert_mints_batch;

use crate::{error::StorageError, storage::postgres::db::PostgresDb};

#[cfg(test)]
pub mod mock;

#[derive(Clone)]
pub enum Storage {
    Postgres(PostgresDb),
    #[cfg(test)]
    Mock(mock::MockStorage),
}

impl Storage {
    /// Initialize database schema
    pub async fn init_schema(&self) -> Result<(), StorageError> {
        init_schema::init_schema(self).await
    }

    /// Drop all database tables
    pub async fn drop_tables(&self) -> Result<(), StorageError> {
        drop_tables::drop_tables(self).await
    }

    /// Insert a new transaction
    pub async fn insert_db_transaction(
        &self,
        transaction: &DbTransaction,
    ) -> Result<i64, StorageError> {
        insert_db_transaction::insert_db_transaction(self, transaction).await
    }

    /// Insert multiple transactions in a batch
    /// Returns the IDs of inserted transactions in the same order
    pub async fn insert_db_transactions_batch(
        &self,
        transactions: &[DbTransaction],
    ) -> Result<Vec<i64>, StorageError> {
        insert_db_transactions_batch::insert_db_transactions_batch(self, transactions).await
    }

    /// Get pending transactions
    pub async fn get_pending_db_transactions(
        &self,
        transaction_type: TransactionType,
        limit: i64,
    ) -> Result<Vec<DbTransaction>, StorageError> {
        get_pending_db_transactions::get_pending_db_transactions(self, transaction_type, limit)
            .await
    }

    /// Get all transactions of a given type regardless of status
    pub async fn get_all_db_transactions(
        &self,
        transaction_type: TransactionType,
        limit: i64,
    ) -> Result<Vec<DbTransaction>, Box<dyn std::error::Error + Send + Sync>> {
        get_all_db_transactions::get_all_db_transactions(self, transaction_type, limit).await
    }

    /// Get and lock pending transactions for processing (FOR UPDATE SKIP LOCKED)
    /// Sets status to Processing and returns locked rows
    pub async fn get_and_lock_pending_transactions(
        &self,
        transaction_type: TransactionType,
        limit: i64,
    ) -> Result<Vec<DbTransaction>, StorageError> {
        get_and_lock_pending_transactions::get_and_lock_pending_transactions(
            self,
            transaction_type,
            limit,
        )
        .await
    }

    /// Get committed checkpoint for a program type
    pub async fn get_committed_checkpoint(
        &self,
        program_type: &str,
    ) -> Result<Option<u64>, StorageError> {
        get_committed_checkpoint::get_committed_checkpoint(self, program_type).await
    }

    /// Update committed checkpoint for a program type
    pub async fn update_committed_checkpoint(
        &self,
        program_type: &str,
        slot: u64,
    ) -> Result<(), StorageError> {
        update_committed_checkpoint::update_committed_checkpoint(self, program_type, slot).await
    }

    /// Update transaction status after processing
    pub async fn update_transaction_status(
        &self,
        transaction_id: i64,
        status: TransactionStatus,
        counterpart_signature: Option<String>,
        processed_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), StorageError> {
        update_transaction_status::update_transaction_status(
            self,
            transaction_id,
            status,
            counterpart_signature,
            processed_at,
        )
        .await
    }

    /// Insert or update multiple mints in a batch (upsert on mint_address)
    pub async fn upsert_mints_batch(&self, mints: &[DbMint]) -> Result<(), StorageError> {
        upsert_mints_batch::upsert_mints_batch(self, mints).await
    }

    /// Get mint metadata by address
    pub async fn get_mint(&self, mint_address: &str) -> Result<Option<DbMint>, StorageError> {
        get_mint::get_mint(self, mint_address).await
    }

    /// Return per-mint aggregate balances (completed deposits minus withdrawals) for startup reconciliation.
    pub async fn get_mint_balances_for_reconciliation(
        &self,
    ) -> Result<Vec<MintDbBalance>, StorageError> {
        get_mint_balances_for_reconciliation::get_mint_balances_for_reconciliation(self).await
    }

    /// Query escrow balances by mint for continuous reconciliation checks.
    /// Only counts **completed** transactions for both deposits and withdrawals.
    /// Returns per-mint aggregate balances where net_balance = total_deposits - total_withdrawals.
    pub async fn get_escrow_balances_by_mint(&self) -> Result<Vec<MintDbBalance>, StorageError> {
        get_escrow_balances_by_mint::get_escrow_balances_by_mint(self).await
    }

    /// Close the storage connection pool gracefully
    /// Waits for active connections to complete and closes the pool
    pub async fn close(&self) -> Result<(), StorageError> {
        close::close(self).await
    }

    pub async fn count_pending_transactions(
        &self,
        transaction_type: TransactionType,
    ) -> Result<i64, StorageError> {
        count_pending_transactions::count_pending_transactions(self, transaction_type).await
    }

    /// Get completed withdrawal nonces in the given range [min_nonce, max_nonce)
    pub async fn get_completed_withdrawal_nonces(
        &self,
        min_nonce: u64,
        max_nonce: u64,
    ) -> Result<Vec<u64>, StorageError> {
        get_completed_withdrawal_nonces::get_completed_withdrawal_nonces(self, min_nonce, max_nonce)
            .await
    }
}

/// MockStorage behavior tests — only test non-trivial mock logic (filtering, recording, failure).
/// Tautological tests (mock returns Ok → assert Ok) are intentionally omitted.
/// Real storage behavior is covered by postgres_db_test.rs integration tests.
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use mock::MockStorage;

    fn make_mock_storage() -> (Storage, MockStorage) {
        let mock = MockStorage::new();
        let storage = Storage::Mock(mock.clone());
        (storage, mock)
    }

    fn make_db_transaction() -> DbTransaction {
        DbTransaction {
            id: 0,
            signature: "test_sig".to_string(),
            trace_id: "trace-1".to_string(),
            slot: 100,
            initiator: "initiator".to_string(),
            recipient: "recipient".to_string(),
            mint: "mint_addr".to_string(),
            amount: 1000,
            memo: None,
            transaction_type: TransactionType::Deposit,
            withdrawal_nonce: None,
            status: TransactionStatus::Pending,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            processed_at: None,
            counterpart_signature: None,
        }
    }

    // ── insert recording + failure ───────────────────────────────────

    #[tokio::test]
    async fn insert_db_transaction_records_and_returns_incremental_ids() {
        let (storage, mock) = make_mock_storage();
        let id1 = storage
            .insert_db_transaction(&make_db_transaction())
            .await
            .unwrap();
        let id2 = storage
            .insert_db_transaction(&make_db_transaction())
            .await
            .unwrap();
        assert_ne!(id1, id2);

        let recorded = mock.inserted_single_transactions.lock().unwrap();
        assert_eq!(recorded.len(), 2);
    }

    #[tokio::test]
    async fn insert_db_transaction_respects_should_fail() {
        let (storage, mock) = make_mock_storage();
        mock.set_should_fail("insert_db_transaction", true);
        assert!(storage
            .insert_db_transaction(&make_db_transaction())
            .await
            .is_err());
    }

    // ── pending transaction filtering ────────────────────────────────

    #[tokio::test]
    async fn get_pending_filters_by_type_and_respects_limit() {
        let (storage, mock) = make_mock_storage();
        {
            let mut pending = mock.pending_transactions.lock().unwrap();
            for i in 0..3 {
                let mut txn = make_db_transaction();
                txn.signature = format!("dep_{i}");
                pending.push(txn);
            }
            let mut w = make_db_transaction();
            w.transaction_type = TransactionType::Withdrawal;
            w.signature = "wd_0".to_string();
            pending.push(w);
        }

        // Only deposits, capped at 2
        let deps = storage
            .get_pending_db_transactions(TransactionType::Deposit, 2)
            .await
            .unwrap();
        assert_eq!(deps.len(), 2);

        // Withdrawal type returns only the withdrawal
        let wds = storage
            .get_pending_db_transactions(TransactionType::Withdrawal, 10)
            .await
            .unwrap();
        assert_eq!(wds.len(), 1);
        assert_eq!(wds[0].signature, "wd_0");
    }

    // ── lock + drain filtering ───────────────────────────────────────

    #[tokio::test]
    async fn get_and_lock_drains_matched_leaves_rest() {
        let (storage, mock) = make_mock_storage();
        {
            let mut pending = mock.pending_transactions.lock().unwrap();
            for i in 0..3 {
                let mut txn = make_db_transaction();
                txn.signature = format!("dep_{i}");
                pending.push(txn);
            }
            let mut w = make_db_transaction();
            w.transaction_type = TransactionType::Withdrawal;
            w.signature = "wd_0".to_string();
            pending.push(w);
        }

        let locked = storage
            .get_and_lock_pending_transactions(TransactionType::Deposit, 2)
            .await
            .unwrap();
        assert_eq!(locked.len(), 2);

        // 1 deposit + 1 withdrawal remain
        {
            let remaining = mock.pending_transactions.lock().unwrap();
            assert_eq!(remaining.len(), 2);
        }
        let locked2 = storage
            .get_and_lock_pending_transactions(TransactionType::Deposit, 10)
            .await
            .unwrap();
        assert_eq!(locked2.len(), 1);
    }

    // ── status update recording ──────────────────────────────────────

    #[tokio::test]
    async fn update_transaction_status_records_params() {
        let (storage, mock) = make_mock_storage();
        let now = Utc::now();
        storage
            .update_transaction_status(
                42,
                TransactionStatus::Completed,
                Some("sig_abc".to_string()),
                now,
            )
            .await
            .unwrap();

        let updates = mock.status_updates.lock().unwrap();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].0, 42);
        assert_eq!(updates[0].1, TransactionStatus::Completed);
        assert_eq!(updates[0].2.as_deref(), Some("sig_abc"));
    }

    #[tokio::test]
    async fn update_transaction_status_respects_should_fail() {
        let (storage, mock) = make_mock_storage();
        mock.set_should_fail("update_transaction_status", true);
        assert!(storage
            .update_transaction_status(1, TransactionStatus::Completed, None, Utc::now())
            .await
            .is_err());
    }
}
