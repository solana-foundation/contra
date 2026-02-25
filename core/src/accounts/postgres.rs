use {
    anyhow::Result,
    solana_sdk::{account::AccountSharedData, pubkey::Pubkey},
    solana_svm_callback::{InvokeContextCallback, TransactionProcessingCallback},
    sqlx::{postgres::PgPoolOptions, PgPool},
    std::sync::Arc,
    tracing::{debug, info},
    url::Url,
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
