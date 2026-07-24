use {
    super::traits::AccountsDB,
    crate::accounts::{PostgresAccountsDB, RedisAccountsDB},
    redis::{AsyncCommands, RedisResult},
    solana_sdk::{account::AccountSharedData, pubkey::Pubkey},
    sqlx::Row,
    std::{fmt::Display, future::Future, sync::Arc, time::Duration},
};

/// A load failure the caller must treat as fatal, kept separate from a genuinely
/// absent account so a failure is never mistaken for a missing account. A loaded
/// account is a plain `Some`, and not-in-the-DB is a plain `None`.
#[derive(Debug, Clone)]
pub enum AccountLoadError {
    /// The query failed and did not recover after retries (transient outage).
    Backend(String),
    /// A stored account could not be deserialized. Retrying cannot fix bad bytes,
    /// so this is an integrity problem that halts the batch.
    Corrupt(Pubkey),
}

impl Display for AccountLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AccountLoadError::Backend(msg) => {
                write!(f, "account store read failed after retries: {}", msg)
            }
            AccountLoadError::Corrupt(pubkey) => {
                write!(f, "account {} is corrupt in the store", pubkey)
            }
        }
    }
}

impl std::error::Error for AccountLoadError {}

/// Bounded-retry parameters for transient whole-query failures. Only the query
/// itself is retried; a corrupt row is never retried.
const LOAD_MAX_ATTEMPTS: u32 = 4;
const LOAD_BACKOFF_BASE_MS: u64 = 20;
const LOAD_BACKOFF_CAP_MS: u64 = 500;

#[cfg(not(test))]
fn retry_params() -> (u32, u64) {
    (LOAD_MAX_ATTEMPTS, LOAD_BACKOFF_BASE_MS)
}

// Tests shrink the retry schedule so the transient-fatal path returns fast.
#[cfg(test)]
static TEST_MAX_ATTEMPTS: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(LOAD_MAX_ATTEMPTS);
#[cfg(test)]
static TEST_BASE_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(LOAD_BACKOFF_BASE_MS);

#[cfg(test)]
fn retry_params() -> (u32, u64) {
    use std::sync::atomic::Ordering::Relaxed;
    (TEST_MAX_ATTEMPTS.load(Relaxed), TEST_BASE_MS.load(Relaxed))
}

#[cfg(test)]
pub(crate) fn set_test_retry(attempts: u32, base_ms: u64) {
    use std::sync::atomic::Ordering::Relaxed;
    TEST_MAX_ATTEMPTS.store(attempts, Relaxed);
    TEST_BASE_MS.store(base_ms, Relaxed);
}

#[cfg(test)]
pub(crate) fn reset_test_retry() {
    set_test_retry(LOAD_MAX_ATTEMPTS, LOAD_BACKOFF_BASE_MS);
}

/// Exponential backoff for `attempt` (1-based), capped at `LOAD_BACKOFF_CAP_MS`.
fn backoff_delay(attempt: u32, base_ms: u64) -> Duration {
    let shift = attempt.saturating_sub(1).min(20);
    let ms = base_ms
        .saturating_mul(1u64 << shift)
        .min(LOAD_BACKOFF_CAP_MS);
    Duration::from_millis(ms)
}

/// Retry `op` up to `attempts` times with capped exponential backoff, awaiting
/// `sleep` between tries. Split from `with_retry` so unit tests can inject a
/// fake sleeper and observe the requested delays without wall-clock waits.
async fn with_retry_using_sleep<T, E, F, Fut, S, SFut>(
    attempts: u32,
    base_ms: u64,
    mut op: F,
    mut sleep: S,
) -> Result<T, E>
where
    E: Display,
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    S: FnMut(Duration) -> SFut,
    SFut: Future<Output = ()>,
{
    let max = attempts.max(1);
    let mut attempt = 1u32;
    loop {
        match op().await {
            Ok(value) => return Ok(value),
            Err(e) => {
                if attempt >= max {
                    return Err(e);
                }
                let delay = backoff_delay(attempt, base_ms);
                tracing::warn!(
                    "account load attempt {}/{} failed: {}; retrying in {:?}",
                    attempt,
                    max,
                    e,
                    delay
                );
                sleep(delay).await;
                attempt += 1;
            }
        }
    }
}

/// Production wrapper over `with_retry_using_sleep` using the configured
/// schedule and a real tokio sleep.
async fn with_retry<T, E, F, Fut>(op: F) -> Result<T, E>
where
    E: Display,
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    let (attempts, base_ms) = retry_params();
    with_retry_using_sleep(attempts, base_ms, op, |d| tokio::time::sleep(d)).await
}

/// Deserialize a stored row. `Err` means the bytes are corrupt, which the caller
/// turns into a fatal error rather than retrying.
fn classify_row(data: &[u8]) -> Result<AccountSharedData, ()> {
    bincode::deserialize::<AccountSharedData>(data).map_err(|_| ())
}

/// Load used by preload: `Some` per found account, `None` for an absent one, and
/// a typed `Err` for either fatal case (transient outage or a corrupt row).
pub async fn load_accounts(
    db: &AccountsDB,
    accounts: &[Pubkey],
) -> Result<Vec<Option<AccountSharedData>>, AccountLoadError> {
    match db {
        AccountsDB::Postgres(postgres_db) => load_accounts_postgres(postgres_db, accounts).await,
        AccountsDB::Redis(redis_db) => load_accounts_redis(redis_db, accounts).await,
    }
}

/// Returns an account when found, or `None` for any other case: not in the DB,
/// corrupt, or a read error. Use when the caller only needs the account value.
pub async fn get_accounts(db: &AccountsDB, accounts: &[Pubkey]) -> Vec<Option<AccountSharedData>> {
    load_accounts(db, accounts)
        .await
        .unwrap_or_else(|_| vec![None; accounts.len()])
}

async fn load_accounts_postgres(
    postgres_db: &PostgresAccountsDB,
    accounts: &[Pubkey],
) -> Result<Vec<Option<AccountSharedData>>, AccountLoadError> {
    let pool = Arc::clone(&postgres_db.pool);
    let pubkey_bytes: Vec<Vec<u8>> = accounts.iter().map(|key| key.to_bytes().to_vec()).collect();

    // Only the whole query is retried; a corrupt row is fatal, handled below.
    let rows = with_retry(|| async {
        sqlx::query("SELECT pubkey, data FROM accounts WHERE pubkey = ANY($1)")
            .bind(&pubkey_bytes)
            .fetch_all(pool.as_ref())
            .await
    })
    .await
    .map_err(|e| AccountLoadError::Backend(e.to_string()))?;

    // Absent keys stay None; a present row that will not deserialize is fatal.
    let mut result = vec![None; accounts.len()];
    for row in rows {
        let row_pubkey: Vec<u8> = row.get("pubkey");
        let data: Vec<u8> = row.get("data");

        if let Some(index) = accounts
            .iter()
            .position(|&key| key.to_bytes().as_slice() == row_pubkey)
        {
            match classify_row(&data) {
                Ok(account) => result[index] = Some(account),
                Err(()) => return Err(AccountLoadError::Corrupt(accounts[index])),
            }
        }
    }
    Ok(result)
}

async fn load_accounts_redis(
    redis_db: &RedisAccountsDB,
    accounts: &[Pubkey],
) -> Result<Vec<Option<AccountSharedData>>, AccountLoadError> {
    let keys = accounts
        .iter()
        .map(|key| format!("account:{}", key))
        .collect::<Vec<_>>();

    let data: Vec<Option<Vec<u8>>> = with_retry(|| async {
        let mut conn = redis_db.connection.clone();
        let out: RedisResult<Vec<Option<Vec<u8>>>> = conn.mget(&keys).await;
        out
    })
    .await
    .map_err(|e| AccountLoadError::Backend(e.to_string()))?;

    let mut result = vec![None; accounts.len()];
    for (index, opt) in data.into_iter().enumerate() {
        if let Some(bytes) = opt {
            match classify_row(&bytes) {
                Ok(account) => result[index] = Some(account),
                Err(()) => return Err(AccountLoadError::Corrupt(accounts[index])),
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::test_helpers::{start_test_postgres, start_test_redis},
        solana_sdk::account::ReadableAccount,
        std::sync::atomic::{AtomicU32, Ordering},
    };

    // ── with_retry unit tests (no wall-clock sleep) ──

    /// A no-op sleeper that records every requested delay so tests can assert on
    /// the backoff schedule without actually waiting.
    fn recording_sleep(
        log: std::rc::Rc<std::cell::RefCell<Vec<Duration>>>,
    ) -> impl FnMut(Duration) -> std::future::Ready<()> {
        move |d: Duration| {
            log.borrow_mut().push(d);
            std::future::ready(())
        }
    }

    #[tokio::test]
    async fn with_retry_succeeds_after_n_failures() {
        let calls = AtomicU32::new(0);
        // Fail twice, succeed on the third call.
        let result: Result<u32, String> = with_retry_using_sleep(
            5,
            1,
            || {
                let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                async move {
                    if n < 3 {
                        Err(format!("transient {n}"))
                    } else {
                        Ok(n)
                    }
                }
            },
            |_d| std::future::ready(()),
        )
        .await;

        assert_eq!(result.unwrap(), 3);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            3,
            "must stop as soon as it succeeds"
        );
    }

    #[tokio::test]
    async fn with_retry_exhausts_and_returns_err() {
        let calls = AtomicU32::new(0);
        let result: Result<u32, String> = with_retry_using_sleep(
            4,
            1,
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async move { Err::<u32, _>("always fails".to_string()) }
            },
            |_d| std::future::ready(()),
        )
        .await;

        assert!(result.is_err());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            4,
            "must invoke exactly LOAD_MAX_ATTEMPTS times, no more, no fewer"
        );
    }

    #[tokio::test]
    async fn with_retry_backoff_is_bounded() {
        let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let result: Result<u32, String> = with_retry_using_sleep(
            6,
            1000, // large base so every delay would exceed the cap if uncapped
            || async { Err::<u32, _>("boom".to_string()) },
            recording_sleep(log.clone()),
        )
        .await;

        assert!(result.is_err());
        let delays = log.borrow();
        // One sleep between each of the 6 attempts → 5 sleeps.
        assert_eq!(delays.len(), 5);
        for d in delays.iter() {
            assert!(
                *d <= Duration::from_millis(LOAD_BACKOFF_CAP_MS),
                "no single backoff may exceed the cap: {:?}",
                d
            );
        }
        // Backoff must be non-decreasing (exponential until the cap).
        for pair in delays.windows(2) {
            assert!(pair[1] >= pair[0], "backoff must not shrink");
        }
    }

    // ── classify_row unit tests ──

    #[test]
    fn classify_row_valid_is_found() {
        let account = AccountSharedData::new(7, 0, &Pubkey::new_unique());
        let bytes = bincode::serialize(&account).unwrap();
        let loaded = classify_row(&bytes).expect("valid bytes must deserialize");
        assert_eq!(loaded.lamports(), 7);
    }

    #[test]
    fn classify_row_garbage_is_corrupt() {
        let garbage = vec![0xffu8; 3];
        assert!(classify_row(&garbage).is_err());
    }

    // ── Postgres integration: load_accounts ──

    /// Overwrite the `data` column for `pubkey` with bytes that cannot
    /// deserialize into an AccountSharedData.
    async fn insert_corrupt_account_pg(db: &AccountsDB, pubkey: Pubkey) {
        let AccountsDB::Postgres(pg) = db else {
            panic!("expected Postgres");
        };
        sqlx::query(
            "INSERT INTO accounts (pubkey, data) VALUES ($1, $2)
             ON CONFLICT (pubkey) DO UPDATE SET data = EXCLUDED.data",
        )
        .bind(pubkey.to_bytes().to_vec())
        .bind(vec![0xAAu8; 5])
        .execute(pg.pool.as_ref())
        .await
        .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn load_accounts_found_missing_mix() {
        let (mut db, _pg) = start_test_postgres().await;
        let stored = Pubkey::new_unique();
        let absent = Pubkey::new_unique();
        db.set_account(stored, AccountSharedData::new(9, 0, &Pubkey::new_unique()))
            .await;

        let loads = load_accounts(&db, &[stored, absent]).await.unwrap();
        assert!(loads[0].is_some());
        assert!(loads[1].is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn load_accounts_corrupt_row_is_fatal() {
        let (db, _pg) = start_test_postgres().await;
        let bad = Pubkey::new_unique();
        insert_corrupt_account_pg(&db, bad).await;

        // A corrupt row is an integrity fault: the whole load fails fatally.
        let result = load_accounts(&db, &[bad]).await;
        assert!(matches!(result, Err(AccountLoadError::Corrupt(k)) if k == bad));
    }

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn load_accounts_transient_error_is_fatal() {
        // Dead backend + shrunk retries: the read must surface a typed fatal
        // error, never a `None`. Serial so the global retry override does not
        // race other transient tests.
        let db = crate::test_helpers::dead_postgres_db();
        set_test_retry(2, 1);
        let result = load_accounts(&db, &[Pubkey::new_unique()]).await;
        reset_test_retry();

        assert!(
            matches!(result, Err(AccountLoadError::Backend(_))),
            "a dead backend must surface a typed fatal error, never None"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn public_get_accounts_adapter_found_and_missing() {
        let (mut db, _pg) = start_test_postgres().await;
        let found = Pubkey::new_unique();
        let missing = Pubkey::new_unique();
        db.set_account(found, AccountSharedData::new(3, 0, &Pubkey::new_unique()))
            .await;

        let results = db.get_accounts(&[found, missing]).await;
        assert!(results[0].is_some(), "found surfaces as Some");
        assert!(results[1].is_none(), "missing surfaces as None");
    }

    // ── Redis integration: load_accounts ──

    async fn insert_corrupt_account_redis(db: &mut RedisAccountsDB, pubkey: Pubkey) {
        use redis::AsyncCommands;
        let key = format!("account:{}", pubkey);
        let _: RedisResult<()> = db.connection.set(key, vec![0xAAu8; 5]).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn load_accounts_redis_found_and_missing() {
        let (mut raw, _redis) = start_test_redis().await;
        let found = Pubkey::new_unique();
        let missing = Pubkey::new_unique();
        raw.set_account(found, AccountSharedData::new(4, 0, &Pubkey::new_unique()))
            .await;

        let db = AccountsDB::Redis(raw);
        let loads = load_accounts(&db, &[found, missing]).await.unwrap();
        assert!(loads[0].is_some());
        assert!(loads[1].is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn load_accounts_redis_corrupt_row_is_fatal() {
        let (mut raw, _redis) = start_test_redis().await;
        let corrupt = Pubkey::new_unique();
        insert_corrupt_account_redis(&mut raw, corrupt).await;

        let db = AccountsDB::Redis(raw);
        let result = load_accounts(&db, &[corrupt]).await;
        assert!(matches!(result, Err(AccountLoadError::Corrupt(k)) if k == corrupt));
    }

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn load_accounts_redis_transient_error_is_fatal() {
        let db = AccountsDB::Redis(crate::test_helpers::dead_redis_db().await);
        set_test_retry(2, 1);
        let result = load_accounts(&db, &[Pubkey::new_unique()]).await;
        reset_test_retry();

        assert!(matches!(result, Err(AccountLoadError::Backend(_))));
    }
}
