use {
    super::{
        traits::SchedulerTrait,
        types::{extract_accounts, ConflictFreeBatch, TransactionWithIndex},
    },
    solana_sdk::transaction::SanitizedTransaction,
    std::{collections::HashSet, sync::Arc},
};

pub struct GreedyScheduler {}

impl Default for GreedyScheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl GreedyScheduler {
    pub fn new() -> Self {
        Self {}
    }
}

impl SchedulerTrait for GreedyScheduler {
    fn schedule(&mut self, transactions: Vec<SanitizedTransaction>) -> Vec<ConflictFreeBatch> {
        if transactions.is_empty() {
            return vec![];
        }

        let mut batches = Vec::new();
        let mut remaining_transactions: Vec<_> = transactions
            .into_iter()
            .enumerate()
            .map(|(index, tx)| TransactionWithIndex {
                transaction: Arc::new(tx),
                index,
            })
            .collect();

        while !remaining_transactions.is_empty() {
            let mut batch = ConflictFreeBatch::new();
            let mut batch_read_accounts = HashSet::new();
            let mut batch_write_accounts = HashSet::new();
            let mut next_remaining = Vec::new();

            for tx_with_index in remaining_transactions {
                let (read_accounts, write_accounts) = extract_accounts(&tx_with_index.transaction);

                // Check for conflicts (same logic as DAG scheduler)
                let mut has_conflict = false;

                // Check write-write conflicts
                for account in &write_accounts {
                    if batch_write_accounts.contains(account) {
                        has_conflict = true;
                        break;
                    }
                }

                // Check read-write conflicts
                if !has_conflict {
                    for account in &write_accounts {
                        if batch_read_accounts.contains(account) {
                            has_conflict = true;
                            break;
                        }
                    }
                }

                if !has_conflict {
                    for account in &read_accounts {
                        if batch_write_accounts.contains(account) {
                            has_conflict = true;
                            break;
                        }
                    }
                }

                if has_conflict {
                    next_remaining.push(tx_with_index);
                } else {
                    for account in &read_accounts {
                        batch_read_accounts.insert(*account);
                    }
                    for account in &write_accounts {
                        batch_write_accounts.insert(*account);
                    }
                    batch.add_transaction(tx_with_index);
                }
            }

            if !batch.is_empty() {
                batches.push(batch);
            }

            remaining_transactions = next_remaining;
        }

        batches
    }
}
