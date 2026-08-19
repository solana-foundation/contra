use {
    super::postgres::PostgresAccountsDB,
    anyhow::Result,
    redis::{aio::ConnectionManager, AsyncCommands, RedisResult},
    solana_sdk::{account::AccountSharedData, pubkey::Pubkey},
    solana_svm_callback::{InvokeContextCallback, TransactionProcessingCallback},
    std::sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
};

#[derive(Clone)]
pub struct RedisAccountsDB {
    pub connection: ConnectionManager,
    /// The Postgres source of truth this cache sits in front of. A key absent
    /// from Redis is a cache miss to be resolved here, never an authoritative
    /// "does not exist".
    ///
    /// Not optional: a cache with no source of truth behind it can only answer
    /// a miss by claiming the data does not exist, which is the one answer it
    /// must never give.
    pub fallback: PostgresAccountsDB,
    /// The deployment the cache must name for its contents to be usable, read
    /// from Postgres once at construction because it never changes for a given
    /// database. Held here so a read can check the stamp without a second
    /// round trip.
    deployment_id: Vec<u8>,
    /// Counts how many times this cache has been taken out of service. A rebuild
    /// carries the value it was started with and declines to stamp if it has
    /// moved on, so a rebuild overtaken by a later condemnation cannot return a
    /// cache to service whose purge predates the newer gap.
    condemnations: Arc<AtomicU64>,
    /// Whether a rebuild is already working on this cache. A condemned cache is
    /// not mirrored to, so its tip stays frozen and every later batch sees the
    /// same discontinuity; without this, each one would condemn again.
    rebuilding: Arc<AtomicBool>,
}

impl RedisAccountsDB {
    pub async fn new(redis_url: &str, fallback: PostgresAccountsDB) -> Result<Self, String> {
        // Parse URL to extract host/port without credentials for error messages
        let sanitized_url = if let Ok(parsed) = url::Url::parse(redis_url) {
            let host = parsed.host_str().unwrap_or("unknown");
            let port = parsed.port().unwrap_or(6379);
            format!("{}:{}", host, port)
        } else {
            "unknown".to_string()
        };

        let client = redis::Client::open(redis_url)
            .map_err(|_| format!("Failed to create Redis client for {}", sanitized_url))?;
        let connection = ConnectionManager::new(client)
            .await
            .map_err(|_| format!("Failed to connect to Redis at {}", sanitized_url))?;

        let deployment_id = super::redis_coherence::read_deployment_id(&fallback)
            .await
            .map_err(|e| format!("{e:#}"))?;

        let db = Self {
            connection,
            fallback,
            deployment_id,
            condemnations: Arc::new(AtomicU64::new(0)),
            rebuilding: Arc::new(AtomicBool::new(false)),
        };
        Ok(db)
    }

    /// The deployment this cache must name to be readable. Renewing the lease
    /// rewrites it, so the mirror path needs it without a Postgres round trip.
    pub(crate) fn deployment_id(&self) -> &[u8] {
        &self.deployment_id
    }

    /// How many times this cache has been taken out of service. Shared across
    /// clones of the handle, so a rebuild running on one clone sees a
    /// condemnation raised on another.
    pub(crate) fn condemnation_generation(&self) -> u64 {
        self.condemnations.load(Ordering::SeqCst)
    }

    /// Records that the cache has been taken out of service, superseding any
    /// rebuild already in flight.
    pub(crate) fn record_condemnation(&self) {
        self.condemnations.fetch_add(1, Ordering::SeqCst);
    }

    /// Claims the right to rebuild this cache, returning false when a rebuild
    /// already holds it. Checking and claiming in one step, so two batches
    /// cannot both decide they are the one to condemn.
    pub(crate) fn try_begin_rebuild(&self) -> bool {
        self.rebuilding
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// Releases the claim. Called however the rebuild ended: a failed one that
    /// held the claim would leave the cache unmirrored and unable to condemn
    /// again, so it would never recover.
    pub(crate) fn finish_rebuild(&self) {
        self.rebuilding.store(false, Ordering::SeqCst);
    }

    /// Reads a cached value and the deployment stamp in one round trip, yielding
    /// the value only while the stamp still names this deployment. Checking the
    /// stamp per read, rather than on a timer, is what makes a condemnation take
    /// effect on the very next read. `MGET` of two keys costs what `GET` of one
    /// costs.
    ///
    /// `None` covers a missing key, a missing stamp and a foreign stamp alike:
    /// all three mean the cache cannot answer, and callers resolve that against
    /// Postgres.
    pub(crate) async fn get_trusted<T: redis::FromRedisValue>(
        &self,
        key: &str,
    ) -> RedisResult<Option<T>> {
        let mut conn = self.connection.clone();
        let (stamp, value): (Option<Vec<u8>>, Option<T>) = redis::cmd("MGET")
            .arg(super::redis_coherence::DEPLOYMENT_ID_KEY)
            .arg(key)
            .query_async(&mut conn)
            .await?;

        match stamp {
            Some(stamp) if stamp == self.deployment_id => Ok(value),
            _ => Ok(None),
        }
    }

    /// Whether a stamp read alongside cached values still names this deployment.
    /// For batch reads that fetch the stamp as part of their own `MGET`.
    pub(crate) fn stamp_is_current(&self, stamp: Option<&Vec<u8>>) -> bool {
        stamp.is_some_and(|stamp| *stamp == self.deployment_id)
    }

    pub async fn set_account(&mut self, pubkey: Pubkey, account: AccountSharedData) {
        let key = format!("account:{}", pubkey);
        let serialized = bincode::serialize(&account).unwrap();
        let _: RedisResult<()> = self.connection.set(key, serialized).await;
    }
}

impl InvokeContextCallback for RedisAccountsDB {}

impl TransactionProcessingCallback for RedisAccountsDB {
    fn get_account_shared_data(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
        let db = super::traits::AccountsDB::Redis(self.clone());
        let pubkey = *pubkey;
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                super::get_account_shared_data::get_account_shared_data(&db, &pubkey).await
            })
        })
    }

    fn account_matches_owners(&self, account: &Pubkey, owners: &[Pubkey]) -> Option<usize> {
        let db = super::traits::AccountsDB::Redis(self.clone());
        let account = *account;
        let owners = owners.to_vec();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                super::account_matches_owners::account_matches_owners(&db, &account, &owners).await
            })
        })
    }
}
