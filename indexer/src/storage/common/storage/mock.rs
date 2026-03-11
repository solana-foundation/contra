use crate::error::StorageError;
use crate::storage::common::models::{
    DbMint, DbTransaction, MintDbBalance, TransactionStatus, TransactionType,
};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Mutex;

/// Recorded status update from `update_transaction_status`.
pub type StatusUpdateRecord = (i64, TransactionStatus, Option<String>, DateTime<Utc>);

/// Tuple of (transaction_id, withdrawal_signature_strings, deadline) — the data persisted when a withdrawal transitions to PendingRemint status.
pub type PendingRemintRecord = (i64, Vec<String>, DateTime<Utc>);

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
    /// Signatures stored per transaction on PendingRemint transition, keyed as (transaction_id, remint_signatures, deadline_at).                                                  
    /// Used in tests to verify the correct withdrawal signatures were persisted.                                                                         
    pub pending_remint_signatures: std::sync::Arc<Mutex<Vec<PendingRemintRecord>>>,
    /// Transactions currently in PendingRemint status, used in tests to simulate startup recovery.
    pub pending_remint_transactions: std::sync::Arc<Mutex<Vec<DbTransaction>>>,
}

impl MockStorage {
    pub fn new() -> Self {
        Self::default()
    }

    fn check_should_fail(&self, operation: &str) -> Result<(), StorageError> {
        if self
            .should_fail
            .lock()
            .unwrap()
            .get(operation)
            .copied()
            .unwrap_or(false)
        {
            return Err(StorageError::DatabaseError {
                message: format!("Simulated {operation} failure"),
            });
        }
        Ok(())
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
        self.check_should_fail("insert_db_transaction")?;
        let mut store = self.inserted_single_transactions.lock().unwrap();
        let id = store.len() as i64 + 1;
        store.push(transaction.clone());
        Ok(id)
    }

    pub async fn insert_db_transactions_batch(
        &self,
        transactions: &[DbTransaction],
    ) -> Result<Vec<i64>, StorageError> {
        self.check_should_fail("insert_db_transactions_batch")?;
        let mut store = self.inserted_transactions.lock().unwrap();
        let base = store.iter().map(|b| b.len()).sum::<usize>() as i64;
        store.push(transactions.to_vec());
        let ids: Vec<i64> = (base + 1..=base + transactions.len() as i64).collect();
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
        self.check_should_fail(program_type)?;
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
        self.check_should_fail("update_transaction_status")?;
        self.status_updates.lock().unwrap().push((
            transaction_id,
            status,
            counterpart_signature,
            processed_at,
        ));
        Ok(())
    }

    pub async fn upsert_mints_batch(&self, mints: &[DbMint]) -> Result<(), StorageError> {
        self.check_should_fail("upsert_mints_batch")?;
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

    pub async fn set_pending_remint(
        &self,
        transaction_id: i64,
        remint_signatures: Vec<String>,
        deadline_at: DateTime<Utc>,
    ) -> Result<(), StorageError> {
        if self
            .should_fail
            .lock()
            .unwrap()
            .get("set_pending_remint")
            .copied()
            .unwrap_or(false)
        {
            return Err(StorageError::DatabaseError {
                message: "Simulated set_pending_remint failure".to_string(),
            });
        }
        self.pending_remint_signatures.lock().unwrap().push((
            transaction_id,
            remint_signatures,
            deadline_at,
        ));
        Ok(())
    }

    pub async fn get_pending_remint_transactions(
        &self,
    ) -> Result<Vec<DbTransaction>, StorageError> {
        let pending = self.pending_remint_transactions.lock().unwrap();
        Ok(pending.clone())
    }
}
