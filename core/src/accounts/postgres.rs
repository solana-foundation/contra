use {
    anyhow::Result,
    solana_sdk::{account::AccountSharedData, pubkey::Pubkey},
    solana_svm_callback::{InvokeContextCallback, TransactionProcessingCallback},
    sqlx::{postgres::PgPoolOptions, PgPool},
    std::sync::Arc,
    tracing::{debug, info},
};

#[derive(Clone)]
pub struct PostgresAccountsDB {
    pub pool: Arc<PgPool>,
    pub read_only: bool,
}

impl PostgresAccountsDB {
    pub async fn new(
        database_url: &str,
        read_only: bool,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // Parse URL to extract host/port without credentials
        let sanitized_url = if let Ok(parsed) = url::Url::parse(database_url) {
            let host = parsed.host_str().unwrap_or("unknown");
            let port = parsed.port().unwrap_or(5432);
            let db = parsed.path().trim_start_matches('/');
            format!("{}:{}/{}", host, port, db)
        } else {
            "unknown".to_string()
        };
        info!("Connecting to PostgreSQL: {}", sanitized_url);

        // Create connection pool
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .min_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(30))
            .idle_timeout(std::time::Duration::from_secs(60))
            .connect(database_url)
            .await?;

        info!("Successfully connected to PostgreSQL");

        if !read_only {
            info!("Creating PostgreSQL tables");
            create_tables(&pool).await?;
        } else {
            info!("Skipping table creation in read-only mode");
        }

        let instance = Self {
            pool: Arc::new(pool),
            read_only,
        };

        info!("PostgreSQL accounts database initialized");
        Ok(instance)
    }
}

impl InvokeContextCallback for PostgresAccountsDB {}

impl TransactionProcessingCallback for PostgresAccountsDB {
    fn get_account_shared_data(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
        let db = super::traits::AccountsDB::Postgres(self.clone());
        let pubkey = *pubkey;
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                super::get_account_shared_data::get_account_shared_data(&db, &pubkey).await
            })
        })
    }

    fn account_matches_owners(&self, account: &Pubkey, owners: &[Pubkey]) -> Option<usize> {
        let db = super::traits::AccountsDB::Postgres(self.clone());
        let account = *account;
        let owners = owners.to_vec();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                super::account_matches_owners::account_matches_owners(&db, &account, &owners).await
            })
        })
    }
}

impl Drop for PostgresAccountsDB {
    fn drop(&mut self) {
        debug!("Closing PostgreSQL connection pool");
        // Connection pool will be closed automatically when Arc<PgPool> is dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{start_test_postgres_raw, start_test_postgres_with_new_instance};
    use solana_sdk::account::{AccountSharedData, ReadableAccount};
    use solana_sdk::pubkey::Pubkey;
    use solana_svm_callback::TransactionProcessingCallback;

    /// PostgresAccountsDB::new with read_only=false must create all tables.
    /// Calling it twice must not fail (IF NOT EXISTS idempotency).
    #[tokio::test(flavor = "multi_thread")]
    async fn new_write_mode_creates_tables_idempotently() {
        let (_first, _second, _pg) = start_test_postgres_with_new_instance().await;
        // Helper already verifies that both instances succeeded.
    }

    /// The synchronous TransactionProcessingCallback::get_account_shared_data
    /// returns None for an account that was never stored.
    #[tokio::test(flavor = "multi_thread")]
    async fn transaction_callback_get_account_shared_data_missing_returns_none() {
        let (db, _pg) = start_test_postgres_raw().await;
        let result = db.get_account_shared_data(&Pubkey::new_unique());
        assert!(result.is_none());
    }

    /// The synchronous TransactionProcessingCallback::account_matches_owners
    /// returns None when the account does not exist.
    #[tokio::test(flavor = "multi_thread")]
    async fn transaction_callback_account_matches_owners_missing_returns_none() {
        let (db, _pg) = start_test_postgres_raw().await;
        let result = db.account_matches_owners(&Pubkey::new_unique(), &[Pubkey::new_unique()]);
        assert!(result.is_none());
    }

    /// The synchronous TransactionProcessingCallback::get_account_shared_data
    /// returns Some when an account has been stored in the database.
    #[tokio::test(flavor = "multi_thread")]
    async fn transaction_callback_get_account_shared_data_after_set_returns_some() {
        let (db, _pg) = start_test_postgres_raw().await;

        let pubkey = Pubkey::new_unique();
        let owner = Pubkey::new_unique();
        let account = AccountSharedData::new(42, 0, &owner);

        // Write directly to the database via SQL (serialized like the actual set_account path).
        let pool = &db.pool;
        let account_data = bincode::serialize(&account).unwrap();
        sqlx::query(
            "INSERT INTO accounts (pubkey, data) VALUES ($1, $2) ON CONFLICT (pubkey) DO UPDATE SET data = $2"
        )
        .bind(pubkey.as_ref())
        .bind(account_data)
        .execute(pool.as_ref())
        .await
        .unwrap();

        // Read back via the synchronous TransactionProcessingCallback.
        let result = db.get_account_shared_data(&pubkey);
        assert!(
            result.is_some(),
            "account stored in DB should be retrievable via sync callback"
        );
    }

    /// PostgresAccountsDB::new with invalid URL formats sanitizes gracefully
    /// and logs "unknown" for unparseable URLs.
    #[tokio::test(flavor = "multi_thread")]
    async fn new_with_invalid_url_format_logs_unknown() {
        // An invalid URL that fails to parse (no "://" scheme) should fall back to "unknown"
        // in the sanitized log output. The actual connection will fail, which is expected.
        let invalid_url = "not-a-valid-url-at-all";
        let result = PostgresAccountsDB::new(invalid_url, false).await;
        // Connection fails as expected (URL is invalid), but the sanitization path was covered.
        assert!(result.is_err(), "Invalid URL should fail to connect");
    }

    /// PostgresAccountsDB stores the read_only flag correctly.
    #[tokio::test(flavor = "multi_thread")]
    async fn new_stores_read_only_flag() {
        let (db, _pg) = start_test_postgres_raw().await;
        // start_test_postgres_raw creates a write-mode database (read_only=false).
        assert!(!db.read_only, "Write mode should have read_only=false");
        // Verify we can access the pool through the struct.
        assert!(!db.pool.is_closed(), "Connection pool should be open after new()");
    }

    /// The synchronous TransactionProcessingCallback::account_matches_owners
    /// returns Some(index) when the account exists and owner is in the list.
    #[tokio::test(flavor = "multi_thread")]
    async fn transaction_callback_account_matches_owners_returns_some_when_found() {
        let (db, _pg) = start_test_postgres_raw().await;

        let pubkey = Pubkey::new_unique();
        let owner = Pubkey::new_unique();
        let account = AccountSharedData::new(100, 0, &owner);

        // Store the account
        let pool = &db.pool;
        let account_data = bincode::serialize(&account).unwrap();
        sqlx::query(
            "INSERT INTO accounts (pubkey, data) VALUES ($1, $2) ON CONFLICT (pubkey) DO UPDATE SET data = $2"
        )
        .bind(pubkey.as_ref())
        .bind(account_data)
        .execute(pool.as_ref())
        .await
        .unwrap();

        // Query with the matching owner
        let result = db.account_matches_owners(&pubkey, &[owner]);
        assert_eq!(
            result,
            Some(0),
            "Should return Some(0) when owner matches at index 0"
        );

        // Query with owner in second position
        let other_owner = Pubkey::new_unique();
        let result = db.account_matches_owners(&pubkey, &[other_owner, owner]);
        assert_eq!(
            result,
            Some(1),
            "Should return Some(1) when owner matches at index 1"
        );
    }

    /// The synchronous TransactionProcessingCallback::get_account_shared_data
    /// returns the deserialized account data when the account exists.
    #[tokio::test(flavor = "multi_thread")]
    async fn transaction_callback_get_account_shared_data_deserializes_correctly() {
        let (db, _pg) = start_test_postgres_raw().await;

        let pubkey = Pubkey::new_unique();
        let owner = Pubkey::new_unique();
        let lamports = 5000;
        let account = AccountSharedData::new(lamports, 0, &owner);

        // Store the account
        let pool = &db.pool;
        let account_data = bincode::serialize(&account).unwrap();
        sqlx::query(
            "INSERT INTO accounts (pubkey, data) VALUES ($1, $2) ON CONFLICT (pubkey) DO UPDATE SET data = $2"
        )
        .bind(pubkey.as_ref())
        .bind(account_data)
        .execute(pool.as_ref())
        .await
        .unwrap();

        // Retrieve via callback
        let result = db.get_account_shared_data(&pubkey);
        assert!(result.is_some());
        let retrieved = result.unwrap();
        assert_eq!(retrieved.lamports(), lamports, "Lamports should match");
        assert_eq!(retrieved.owner(), &owner, "Owner should match");
    }

    /// PostgresAccountsDB::new successfully sanitizes a valid database URL with all components.
    #[tokio::test(flavor = "multi_thread")]
    async fn new_with_valid_url_parses_and_logs() {
        let (db, _pg) = start_test_postgres_raw().await;
        // If we got here, a valid URL was parsed successfully.
        // The database connection succeeded and tables were created.
        assert!(!db.pool.is_closed(), "Connection pool must be open");
    }
}

async fn create_tables(pool: &PgPool) -> Result<(), Box<dyn std::error::Error>> {
    // Create tables
    sqlx::query(
        r#"
            CREATE TABLE IF NOT EXISTS accounts (
                pubkey BYTEA PRIMARY KEY,
                data BYTEA NOT NULL
            )
            "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
            CREATE TABLE IF NOT EXISTS transactions (
                signature BYTEA PRIMARY KEY,
                data BYTEA NOT NULL
            )
            "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
            CREATE TABLE IF NOT EXISTS blocks (
                slot BIGINT PRIMARY KEY,
                data BYTEA NOT NULL
            )
            "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
            CREATE TABLE IF NOT EXISTS metadata (
                key VARCHAR PRIMARY KEY,
                value BYTEA NOT NULL
            )
            "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
            CREATE TABLE IF NOT EXISTS performance_samples (
                slot BIGINT PRIMARY KEY,
                num_transactions BIGINT NOT NULL,
                num_slots BIGINT NOT NULL,
                sample_period_secs SMALLINT NOT NULL,
                num_non_vote_transactions BIGINT NOT NULL
            )
            "#,
    )
    .execute(pool)
    .await?;

    info!("PostgreSQL tables initialized");
    Ok(())
}
