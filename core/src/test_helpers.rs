use crate::accounts::traits::BlockInfo;
use solana_sdk::{
    hash::Hash,
    message::Message,
    signature::{Keypair, Signer},
    transaction::{SanitizedTransaction, Transaction},
};
use solana_system_interface::instruction as system_instruction;
use std::collections::HashSet;

/// Create a SanitizedTransaction transferring SOL between two keypairs.
pub fn create_test_sanitized_transaction(
    from: &Keypair,
    to: &solana_sdk::pubkey::Pubkey,
    amount: u64,
) -> SanitizedTransaction {
    let instruction = system_instruction::transfer(&from.pubkey(), to, amount);
    let message = Message::new(&[instruction], Some(&from.pubkey()));
    let transaction = Transaction::new(&[from], message, Hash::default());
    SanitizedTransaction::try_from_legacy_transaction(transaction, &HashSet::new())
        .expect("failed to create SanitizedTransaction from test legacy transaction")
}

/// Create a BlockInfo with sensible defaults for a given slot.
pub fn create_test_block_info(slot: u64, blockhash: Hash) -> BlockInfo {
    BlockInfo {
        slot,
        blockhash,
        previous_blockhash: Hash::default(),
        parent_slot: slot.saturating_sub(1),
        block_height: Some(slot),
        block_time: Some(1_700_000_000 + slot as i64),
        transaction_signatures: vec![],
        transaction_recent_blockhashes: vec![],
    }
}

/// Create a BOB with empty state and a dummy (non-connecting) Postgres pool.
/// The pool uses a bogus URL — any accidental DB call will fail with a
/// connection timeout. Only for unit tests that stay in-memory.
#[cfg(test)]
pub(crate) fn create_test_bob() -> (
    crate::accounts::bob::BOB,
    tokio::sync::mpsc::UnboundedSender<
        Vec<(solana_sdk::pubkey::Pubkey, crate::stages::AccountSettlement)>,
    >,
) {
    use crate::accounts::{AccountsDB, PostgresAccountsDB};
    use sqlx::postgres::PgPoolOptions;
    use std::sync::Arc;

    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://test@localhost:1/test")
        .expect("connect_lazy should not fail");
    let db = AccountsDB::Postgres(PostgresAccountsDB {
        pool: Arc::new(pool),
        read_only: true,
    });
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let bob = crate::accounts::bob::BOB::new_test(rx, db);
    (bob, tx)
}
