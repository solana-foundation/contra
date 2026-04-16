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

/// Spin up a throwaway Postgres container and return a write-mode AccountsDB.
/// The container handle is returned so the caller keeps it alive for the test duration.
#[cfg(test)]
pub(crate) async fn start_test_postgres() -> (
    crate::accounts::AccountsDB,
    testcontainers::ContainerAsync<testcontainers_modules::postgres::Postgres>,
) {
    use testcontainers::runners::AsyncRunner;
    use testcontainers_modules::postgres::Postgres;

    let container = Postgres::default()
        .with_db_name("test_db")
        .with_user("postgres")
        .with_password("password")
        .start()
        .await
        .unwrap();
    let host = container.get_host().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:password@{}:{}/test_db", host, port);
    let db = crate::accounts::AccountsDB::new(&url, false).await.unwrap();
    (db, container)
}

/// Spin up a throwaway Postgres container and return a `PostgresAccountsDB` directly.
/// Use this when testing `PostgresAccountsDB`-specific methods (e.g. `TransactionProcessingCallback`).
#[cfg(test)]
pub(crate) async fn start_test_postgres_raw() -> (
    crate::accounts::PostgresAccountsDB,
    testcontainers::ContainerAsync<testcontainers_modules::postgres::Postgres>,
) {
    use testcontainers::runners::AsyncRunner;
    use testcontainers_modules::postgres::Postgres;

    let container = Postgres::default()
        .with_db_name("pg_test")
        .with_user("postgres")
        .with_password("password")
        .start()
        .await
        .unwrap();
    let host = container.get_host().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:password@{}:{}/pg_test", host, port);
    let db = crate::accounts::PostgresAccountsDB::new(&url, false)
        .await
        .unwrap();
    (db, container)
}

/// Return the connection URL for an already-running testcontainers Postgres instance.
/// Useful when a test needs the raw URL (e.g. to pass to `run_node` or a worker).
#[cfg(test)]
pub(crate) async fn postgres_container_url(
    container: &testcontainers::ContainerAsync<testcontainers_modules::postgres::Postgres>,
    db_name: &str,
) -> String {
    let host = container.get_host().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    format!("postgres://postgres:password@{}:{}/{}", host, port, db_name)
}

/// Start a Postgres container and create two `PostgresAccountsDB` instances
/// pointing to the same container (second one via .new() for idempotency testing).
/// Useful for tests that verify database operations are idempotent.
#[cfg(test)]
pub(crate) async fn start_test_postgres_with_new_instance() -> (
    crate::accounts::PostgresAccountsDB,
    crate::accounts::PostgresAccountsDB,
    testcontainers::ContainerAsync<testcontainers_modules::postgres::Postgres>,
) {
    let (db, container) = start_test_postgres_raw().await;
    let url = postgres_container_url(&container, "pg_test").await;
    let second = crate::accounts::PostgresAccountsDB::new(&url, false)
        .await
        .unwrap();
    (db, second, container)
}

/// Spin up a throwaway Redis container and return a `RedisAccountsDB` directly.
/// Use this when testing `RedisAccountsDB`-specific methods or warm_redis_cache.
#[cfg(test)]
pub(crate) async fn start_test_redis() -> (
    crate::accounts::RedisAccountsDB,
    testcontainers::ContainerAsync<testcontainers_modules::redis::Redis>,
) {
    use testcontainers::{runners::AsyncRunner, ImageExt};

    let container = testcontainers_modules::redis::Redis::default()
        .with_tag("7.0")
        .start()
        .await
        .unwrap();
    let host = container.get_host().await.unwrap();
    let port = container.get_host_port_ipv4(6379).await.unwrap();
    let url = format!("redis://{}:{}", host, port);
    let db = crate::accounts::RedisAccountsDB::new(&url).await.unwrap();
    (db, container)
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
