use crate::error::StorageError;
use crate::storage::common::models::{
    DbMint, DbTransaction, MintDbBalance, TransactionStatus, TransactionType,
};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Mutex;

/// Recorded status update from `update_transaction_status`.
pub type StatusUpdateRecord = (i64, TransactionStatus, Option<String>, DateTime<Utc>);

#[derive(Clone, Default)]
pub struct MockStorage {
    pub committed_checkpoints: std::sync::Arc<Mutex<HashMap<String, u64>>>,
    pub should_fail: std::sync::Arc<Mutex<HashMap<String, bool>>>,
    pub mints: std::sync::Arc<Mutex<HashMap<String, DbMint>>>,
    pub mint_balances: std::sync::Arc<Mutex<Vec<MintDbBalance>>>,
    pub pending_transactions: std::sync::Arc<Mutex<Vec<DbTransaction>>>,
    pub inserted_transactions: std::sync::Arc<Mutex<Vec<Vec<DbTransaction>>>>,
    pub inserted_single_transactions: std::sync::Arc<Mutex<Vec<DbTransaction>>>,
    pub status_updates: std::sync::Arc<Mutex<Vec<StatusUpdateRecord>>>,
}

impl MockStorage {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_checkpoint(&self, program_type: &str, slot: u64) {
        self.committed_checkpoints
            .lock()
            .unwrap()
            .insert(program_type.to_string(), slot);
    }

    pub fn set_should_fail(&self, program_type: &str, should_fail: bool) {
        self.should_fail
            .lock()
            .unwrap()
            .insert(program_type.to_string(), should_fail);
    }

    pub fn add_mint(&mut self, mint: DbMint) {
        self.mints
            .lock()
            .unwrap()
            .insert(mint.mint_address.clone(), mint);
    }

    pub async fn init_schema(&self) -> Result<(), StorageError> {
        Ok(())
    }

    pub async fn drop_tables(&self) -> Result<(), StorageError> {
        Ok(())
    }

    pub async fn insert_db_transaction(
        &self,
        transaction: &DbTransaction,
    ) -> Result<i64, StorageError> {
        if self
            .should_fail
            .lock()
            .unwrap()
            .get("insert_db_transaction")
            .copied()
            .unwrap_or(false)
        {
            return Err(StorageError::DatabaseError {
                message: "Simulated insert_db_transaction failure".to_string(),
            });
        }
        let mut store = self.inserted_single_transactions.lock().unwrap();
        let id = store.len() as i64 + 1;
        store.push(transaction.clone());
        Ok(id)
    }

    pub async fn insert_db_transactions_batch(
        &self,
        transactions: &[DbTransaction],
    ) -> Result<Vec<i64>, StorageError> {
        if self
            .should_fail
            .lock()
            .unwrap()
            .get("insert_db_transactions_batch")
            .copied()
            .unwrap_or(false)
        {
            return Err(StorageError::DatabaseError {
                message: "Simulated insert_db_transactions_batch failure".to_string(),
            });
        }
        self.inserted_transactions
            .lock()
            .unwrap()
            .push(transactions.to_vec());
        let ids: Vec<i64> = (1..=transactions.len() as i64).collect();
        Ok(ids)
    }

    pub async fn get_pending_db_transactions(
        &self,
        transaction_type: TransactionType,
        limit: i64,
    ) -> Result<Vec<DbTransaction>, StorageError> {
        let pending = self.pending_transactions.lock().unwrap();
        let result: Vec<DbTransaction> = pending
            .iter()
            .filter(|t| t.transaction_type == transaction_type)
            .take(limit as usize)
            .cloned()
            .collect();
        Ok(result)
    }

    pub async fn get_and_lock_pending_transactions(
        &self,
        transaction_type: TransactionType,
        limit: i64,
    ) -> Result<Vec<DbTransaction>, StorageError> {
        let mut pending = self.pending_transactions.lock().unwrap();
        let mut matched = Vec::new();
        let mut remaining = Vec::new();

        for txn in pending.drain(..) {
            if txn.transaction_type == transaction_type && (matched.len() as i64) < limit {
                matched.push(txn);
            } else {
                remaining.push(txn);
            }
        }

        *pending = remaining;
        Ok(matched)
    }

    pub async fn get_committed_checkpoint(
        &self,
        program_type: &str,
    ) -> Result<Option<u64>, StorageError> {
        Ok(self
            .committed_checkpoints
            .lock()
            .unwrap()
            .get(program_type)
            .copied())
    }

    pub async fn update_committed_checkpoint(
        &self,
        program_type: &str,
        slot: u64,
    ) -> Result<(), StorageError> {
        // Check if this program type should fail
        if self
            .should_fail
            .lock()
            .unwrap()
            .get(program_type)
            .copied()
            .unwrap_or(false)
        {
            return Err(StorageError::DatabaseError {
                message: "Simulated storage failure".to_string(),
            });
        }

        self.committed_checkpoints
            .lock()
            .unwrap()
            .insert(program_type.to_string(), slot);
        Ok(())
    }

    pub async fn update_transaction_status(
        &self,
        transaction_id: i64,
        status: TransactionStatus,
        counterpart_signature: Option<String>,
        processed_at: DateTime<Utc>,
    ) -> Result<(), StorageError> {
        if self
            .should_fail
            .lock()
            .unwrap()
            .get("update_transaction_status")
            .copied()
            .unwrap_or(false)
        {
            return Err(StorageError::DatabaseError {
                message: "Simulated update_transaction_status failure".to_string(),
            });
        }
        self.status_updates.lock().unwrap().push((
            transaction_id,
            status,
            counterpart_signature,
            processed_at,
        ));
        Ok(())
    }

    pub async fn upsert_mints_batch(&self, mints: &[DbMint]) -> Result<(), StorageError> {
        if self
            .should_fail
            .lock()
            .unwrap()
            .get("upsert_mints_batch")
            .copied()
            .unwrap_or(false)
        {
            return Err(StorageError::DatabaseError {
                message: "Simulated upsert_mints_batch failure".to_string(),
            });
        }
        let mut store = self.mints.lock().unwrap();
        for mint in mints {
            store.insert(mint.mint_address.clone(), mint.clone());
        }
        Ok(())
    }

    pub async fn get_mint(&self, mint_address: &str) -> Result<Option<DbMint>, StorageError> {
        Ok(self.mints.lock().unwrap().get(mint_address).cloned())
    }

    pub fn set_mint_balances(&self, balances: Vec<MintDbBalance>) {
        *self.mint_balances.lock().unwrap() = balances;
    }

    pub async fn get_mint_balances_for_reconciliation(
        &self,
    ) -> Result<Vec<MintDbBalance>, StorageError> {
        Ok(self.mint_balances.lock().unwrap().clone())
    }

    pub async fn get_escrow_balances_by_mint(&self) -> Result<Vec<MintDbBalance>, StorageError> {
        Ok(self.mint_balances.lock().unwrap().clone())
    }

    pub async fn close(&self) -> Result<(), StorageError> {
        Ok(())
    }

    pub async fn count_pending_transactions(
        &self,
        _transaction_type: TransactionType,
    ) -> Result<i64, StorageError> {
        Ok(0)
    }

    pub fn get_completed_withdrawal_nonces(
        &self,
        _min_nonce: u64,
        _max_nonce: u64,
    ) -> Result<Vec<u64>, StorageError> {
        Ok(vec![])
    }
}
