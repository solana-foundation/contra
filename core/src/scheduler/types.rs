use {
    solana_sdk::{pubkey::Pubkey, transaction::SanitizedTransaction},
    std::sync::Arc,
};

#[derive(Debug, Clone)]
pub struct TransactionWithIndex {
    pub transaction: Arc<SanitizedTransaction>,
    pub index: usize,
}

#[derive(Debug)]
pub struct ConflictFreeBatch {
    pub transactions: Vec<TransactionWithIndex>,
}

impl Default for ConflictFreeBatch {
    fn default() -> Self {
        Self::new()
    }
}

impl ConflictFreeBatch {
    pub fn new() -> Self {
        Self {
            transactions: Vec::new(),
        }
    }

    pub fn add_transaction(&mut self, tx: TransactionWithIndex) {
        self.transactions.push(tx);
    }

    pub fn is_empty(&self) -> bool {
        self.transactions.is_empty()
    }

    pub fn len(&self) -> usize {
        self.transactions.len()
    }
}

pub fn extract_accounts(transaction: &SanitizedTransaction) -> (Vec<Pubkey>, Vec<Pubkey>) {
    // Use Solana's built-in account locking mechanism
    // We use usize::MAX as the limit to avoid artificial restrictions
    let account_locks = transaction
        .get_account_locks(usize::MAX)
        .expect("Failed to get account locks");

    let read_accounts = account_locks.readonly.iter().map(|&k| *k).collect();
    let write_accounts = account_locks.writable.iter().map(|&k| *k).collect();

    (read_accounts, write_accounts)
}
