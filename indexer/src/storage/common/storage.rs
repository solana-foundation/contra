pub use super::models::*;

pub mod close;
pub mod drop_tables;
pub mod get_all_db_transactions;
pub mod get_and_lock_pending_transactions;
pub mod get_committed_checkpoint;
pub mod get_completed_withdrawal_nonces;
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

    /// Close the storage connection pool gracefully
    /// Waits for active connections to complete and closes the pool
    pub async fn close(&self) -> Result<(), StorageError> {
        close::close(self).await
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
