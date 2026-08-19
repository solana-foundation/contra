//! Decides when the Redis cache may be served from, and rebuilds it when not.
//!
//! Redis outlives the process that fills it. Point a write node at a fresh,
//! restored or simply different Postgres database while reusing the same Redis
//! instance and every key from the previous ledger is still there, indexed the
//! same way, indistinguishable from current state. A cache can also fall behind
//! the ledger it belongs to: mirroring is best-effort, so an outage leaves its
//! keys holding values Postgres has already moved past.
//!
//! Both are handled by one mechanism. Each Postgres database is stamped with an
//! identifier at creation and the cache carries a copy, which every cached read
//! checks against the identifier the handle was built with. Clearing that stamp
//! therefore takes the cache out of service on the next read, with no window and
//! nothing to notify:
//!
//! - [`staleness_reason`] compares a cache against Postgres at startup, where a
//!   mismatch means another ledger's data or a tip that does not line up.
//! - [`ensure_cache_continuity`] watches for missed batches between blocks, where
//!   the cached tip failing to advance by one slot is the evidence.
//! - [`rebuild_cache`] empties a cache and stamps it, putting it back in service.

use {
    super::{postgres::PostgresAccountsDB, redis::RedisAccountsDB},
    anyhow::{anyhow, Context, Result},
    redis::AsyncCommands,
    tracing::{info, warn},
};

/// Names the deployment the data belongs to. Lives in the Postgres `metadata`
/// table and is mirrored to the same key in Redis once the cache is aligned. Its
/// absence from Redis means the cache has never been checked against a source of
/// truth.
pub(crate) const DEPLOYMENT_ID_KEY: &str = "deployment_id";

/// Cached ledger state, keyed by pubkey, signature, slot or address. `addr_sigs:`
/// is no longer written, but a cache filled by an older build still holds it and
/// a purge has to clear it.
const LEDGER_KEY_PREFIXES: [&str; 4] = ["account:", "tx:", "block:", "addr_sigs:"];

/// Cached ledger state under fixed keys: the chain tip, plus the slot index,
/// transaction counter and performance-sample list that older builds wrote.
const LEDGER_FIXED_KEYS: [&str; 5] = [
    "block_slot_index",
    "transaction_count",
    "performance_samples",
    "latest_slot",
    "latest_blockhash",
];

/// Keys per SCAN round. Bounds how long one round blocks the Redis event loop.
const SCAN_COUNT: usize = 512;

/// Reads the identifier Postgres was stamped with at schema creation.
pub(crate) async fn read_deployment_id(db: &PostgresAccountsDB) -> Result<Vec<u8>> {
    sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM metadata WHERE key = $1")
        .bind(DEPLOYMENT_ID_KEY)
        .fetch_optional(db.pool.as_ref())
        .await
        .context("Failed to read deployment id from Postgres")?
        .ok_or_else(|| anyhow!("Postgres has no deployment id; schema initialization did not run"))
}

/// Why the cache cannot be trusted, or `None` when it is coherent with
/// Postgres.
pub(crate) async fn staleness_reason(
    redis_db: &RedisAccountsDB,
    deployment_id: &[u8],
    postgres_slot: Option<u64>,
) -> Result<Option<String>> {
    let mut conn = redis_db.connection.clone();

    let cached_id: Option<Vec<u8>> = conn
        .get(DEPLOYMENT_ID_KEY)
        .await
        .context("Failed to read deployment id from Redis")?;

    // An empty cache cannot serve anything wrongly, so there is nothing to purge
    // and nothing to check. This is the normal first attach.
    if !holds_ledger_keys(redis_db).await? {
        return Ok(None);
    }

    let Some(cached_id) = cached_id else {
        // Ledger state filled in by something that never checked itself against
        // a source of truth.
        return Ok(Some(
            "cache holds ledger keys but names no deployment".to_string(),
        ));
    };
    if cached_id.as_slice() != deployment_id {
        return Ok(Some(format!(
            "cache belongs to deployment {} but Postgres is {}",
            hex::encode(&cached_id),
            hex::encode(deployment_id)
        )));
    }

    // Postgres with no blocks cannot back any cached ledger state, whatever the
    // tips say. Checked separately because two absent tips compare equal below,
    // so a cache that lost only its tip key would otherwise survive a Postgres
    // that was truncated or replaced.
    if postgres_slot.is_none() {
        return Ok(Some(
            "Postgres holds no blocks but the cache holds ledger keys".to_string(),
        ));
    }

    // The tips must match exactly, not merely not-run-ahead.
    //
    // A cache behind Postgres is not a cache with missing entries: its keys are
    // present, holding the values they had at the cached tip. Reads hit them and
    // never reach the fallback, so a stale balance is served as current. The
    // settler commits to Postgres before mirroring to Redis, so a kill between
    // the two, or a Redis outage, leaves exactly that state.
    //
    // Equality holds after a clean shutdown, so this costs nothing in the normal
    // case and a cold cache otherwise. Cold is slow; behind is wrong.
    let cached_slot: Option<u64> = conn
        .get("latest_slot")
        .await
        .context("Failed to read cached tip from Redis")?;
    if cached_slot != postgres_slot {
        return Ok(Some(format!(
            "cached tip {:?} does not match the Postgres tip {:?}",
            cached_slot, postgres_slot
        )));
    }

    Ok(None)
}

/// Detects that the cache has missed settled batches, and takes it out of
/// service when it has.
///
/// Call before mirroring the batch for `slot`. The tip is written by the same
/// best-effort pipeline as the data, so when that pipeline fails the tip stops
/// advancing along with everything else, and the accounts that batch touched are
/// left holding their pre-batch values. A cached tip that is not exactly one slot
/// back is therefore evidence that batches were missed and that every key in the
/// cache is suspect.
///
/// The response is to clear the deployment stamp, which takes the cache out of
/// service on the next read. Returns whether a rebuild is needed; the caller runs
/// one off the block-production path, because a SCAN over a large keyspace costs
/// far more than a blocktime allows.
///
/// Mirroring continues while the rebuild runs. Nothing is served from an
/// unstamped cache, and `SCAN` returns every key present for the whole pass, so
/// the rebuild clears the stale ones regardless of what is written alongside it.
/// Keys written during the pass may be swept too, which only costs a miss.
pub(crate) async fn ensure_cache_continuity(redis_db: &RedisAccountsDB, slot: u64) -> Result<bool> {
    let mut conn = redis_db.connection.clone();
    let cached_slot: Option<u64> = conn
        .get("latest_slot")
        .await
        .context("Failed to read cached tip from Redis")?;

    // No tip: an empty or freshly purged cache, with nothing to be contiguous
    // with and nothing to lose.
    let Some(cached_slot) = cached_slot else {
        return Ok(false);
    };
    if cached_slot + 1 == slot {
        return Ok(false);
    }

    warn!(
        "Redis cache missed at least one batch (cached tip {}, now settling {}), rebuilding it",
        cached_slot, slot
    );
    // Recorded before the stamp is cleared so a rebuild started for this
    // condemnation cannot be handed a generation that already looks stale.
    redis_db.record_condemnation();
    clear_deployment_id(redis_db).await?;
    Ok(true)
}

/// Empties a condemned cache and then stamps it, which puts it back into service.
///
/// The order is load-bearing: stamping first would return stale keys to service
/// for as long as the purge took. Runs off the block-production path, so the
/// purge is free to walk the whole keyspace.
///
/// `generation` is the condemnation count this rebuild was started for. A cache
/// condemned again while this one was purging has a newer rebuild behind it whose
/// purge covers the newer gap, and stamping here would put keys back in service
/// that only that later purge removes. So a superseded rebuild finishes its purge,
/// which is never wasted work, and leaves the stamping to its successor.
pub(crate) async fn rebuild_cache(redis_db: &RedisAccountsDB, generation: u64) -> Result<()> {
    let deployment_id = read_deployment_id(&redis_db.fallback).await?;
    purge_ledger_keys(redis_db).await?;

    if redis_db.condemnation_generation() != generation {
        info!("Redis cache was condemned again while rebuilding; a later rebuild will finish it");
        return Ok(());
    }

    stamp_deployment_id(redis_db, &deployment_id).await?;
    info!("Redis cache rebuilt and back in service");
    Ok(())
}

/// Removes every cached ledger key, leaving the cache empty rather than wrong.
/// Reads then miss and resolve against Postgres.
pub(crate) async fn purge_ledger_keys(redis_db: &RedisAccountsDB) -> Result<()> {
    let mut conn = redis_db.connection.clone();
    let mut purged = 0usize;

    for prefix in LEDGER_KEY_PREFIXES {
        let pattern = format!("{}*", prefix);
        let mut cursor = 0u64;
        loop {
            let (next_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(SCAN_COUNT)
                .query_async(&mut conn)
                .await
                .with_context(|| format!("Failed to scan {} keys in Redis", pattern))?;

            if !keys.is_empty() {
                purged += keys.len();
                // UNLINK reclaims memory on a background thread, so purging a
                // large keyspace does not stall the Redis event loop.
                let mut unlink = redis::cmd("UNLINK");
                for key in &keys {
                    unlink.arg(key);
                }
                let _: () = unlink
                    .query_async(&mut conn)
                    .await
                    .with_context(|| format!("Failed to unlink {} keys in Redis", pattern))?;
            }

            cursor = next_cursor;
            if cursor == 0 {
                break;
            }
        }
    }

    let mut unlink = redis::cmd("UNLINK");
    for key in LEDGER_FIXED_KEYS {
        unlink.arg(key);
    }
    let _: () = unlink
        .query_async(&mut conn)
        .await
        .context("Failed to unlink cached ledger metadata in Redis")?;

    info!(
        "Purged {} cached ledger keys",
        purged + LEDGER_FIXED_KEYS.len()
    );
    Ok(())
}

/// Marks the cache as belonging to this deployment. Written only after the cache
/// is known to be coherent, and cleared before a purge, so an interrupted purge
/// leaves the cache unstamped rather than falsely stamped.
pub(crate) async fn stamp_deployment_id(
    redis_db: &RedisAccountsDB,
    deployment_id: &[u8],
) -> Result<()> {
    let mut conn = redis_db.connection.clone();
    conn.set::<_, _, ()>(DEPLOYMENT_ID_KEY, deployment_id)
        .await
        .context("Failed to write deployment id to Redis")
}

/// Whether a read node should start against this cache.
///
/// The write node aligns and stamps the cache; a read node has no business
/// purging anything, so it only decides whether what it finds is usable. Usable
/// means either the stamp for this deployment, or no ledger state at all, which
/// cannot answer anything wrongly. Anything else is another ledger's data or the
/// residue of an interrupted rebuild, and the two are indistinguishable here.
///
/// A startup gate only. Individual reads check the stamp again for themselves, so
/// this is about refusing to start against a misconfigured cache rather than
/// about keeping stale values out of responses.
pub(crate) async fn verify_cache_stamp(redis_db: &RedisAccountsDB) -> Result<()> {
    let deployment_id = read_deployment_id(&redis_db.fallback).await?;

    let mut conn = redis_db.connection.clone();
    let cached_id: Option<Vec<u8>> = conn
        .get(DEPLOYMENT_ID_KEY)
        .await
        .context("Failed to read deployment id from Redis")?;

    match cached_id {
        Some(cached_id) if cached_id.as_slice() == deployment_id => Ok(()),
        Some(cached_id) => Err(anyhow!(
            "Redis cache belongs to deployment {} but Postgres is {}",
            hex::encode(&cached_id),
            hex::encode(&deployment_id)
        )),
        None if holds_ledger_keys(redis_db).await? => Err(anyhow!(
            "Redis cache holds ledger keys but names no deployment; it has not been \
             aligned with Postgres"
        )),
        None => Ok(()),
    }
}

pub(crate) async fn clear_deployment_id(redis_db: &RedisAccountsDB) -> Result<()> {
    let mut conn = redis_db.connection.clone();
    conn.del::<_, ()>(DEPLOYMENT_ID_KEY)
        .await
        .context("Failed to clear deployment id in Redis")
}

/// Whether the cache holds any ledger state at all.
async fn holds_ledger_keys(redis_db: &RedisAccountsDB) -> Result<bool> {
    let mut conn = redis_db.connection.clone();

    for key in LEDGER_FIXED_KEYS {
        let exists: bool = conn
            .exists(key)
            .await
            .with_context(|| format!("Failed to check {} in Redis", key))?;
        if exists {
            return Ok(true);
        }
    }

    for prefix in LEDGER_KEY_PREFIXES {
        let pattern = format!("{}*", prefix);
        let mut cursor = 0u64;
        // A single SCAN round may return nothing while keys still exist, so walk
        // until the cursor wraps. Exits on the first hit, and an empty keyspace
        // wraps immediately.
        loop {
            let (next_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(SCAN_COUNT)
                .query_async(&mut conn)
                .await
                .with_context(|| format!("Failed to scan {} keys in Redis", pattern))?;

            if !keys.is_empty() {
                return Ok(true);
            }

            cursor = next_cursor;
            if cursor == 0 {
                break;
            }
        }
    }

    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        accounts::traits::AccountsDB,
        test_helpers::{start_test_postgres, start_test_redis},
    };
    use solana_sdk::pubkey::Pubkey;

    /// Returns a cache handle plus its Postgres source of truth, both throwaway.
    /// The containers are returned so the caller keeps them alive.
    async fn start_cache() -> (
        RedisAccountsDB,
        PostgresAccountsDB,
        testcontainers::ContainerAsync<testcontainers_modules::postgres::Postgres>,
        testcontainers::ContainerAsync<testcontainers_modules::redis::Redis>,
    ) {
        let (pg_db, pg_container) = start_test_postgres().await;
        let AccountsDB::Postgres(ref postgres_db) = pg_db else {
            panic!("expected Postgres variant")
        };
        let postgres_db = postgres_db.clone();
        let (redis_db, redis_container) = start_test_redis(postgres_db.clone()).await;
        (redis_db, postgres_db, pg_container, redis_container)
    }

    /// The normal case: each batch follows the one the cache last recorded, so
    /// nothing was missed and the cache keeps everything it holds.
    #[tokio::test(flavor = "multi_thread")]
    async fn continuity_keeps_a_cache_whose_tip_is_one_slot_back() {
        let (redis_db, postgres_db, _pg, _redis) = start_cache().await;
        let deployment_id = read_deployment_id(&postgres_db).await.unwrap();
        stamp_deployment_id(&redis_db, &deployment_id)
            .await
            .unwrap();

        let cached_pubkey = Pubkey::new_unique();
        let cached_tip = 100u64;
        let mut conn = redis_db.connection.clone();
        let _: () = conn.set("latest_slot", cached_tip).await.unwrap();
        let _: () = conn
            .set(format!("account:{}", cached_pubkey), vec![1u8, 2, 3])
            .await
            .unwrap();

        assert!(
            !ensure_cache_continuity(&redis_db, cached_tip + 1)
                .await
                .unwrap(),
            "a contiguous batch needs no rebuild"
        );

        let still_cached: bool = conn
            .exists(format!("account:{}", cached_pubkey))
            .await
            .unwrap();
        assert!(still_cached, "a contiguous batch must not purge the cache");
    }

    /// The outage case: writes failed for a stretch, so the cached tip stopped
    /// advancing while Postgres kept going. Every key in the cache may now be
    /// stale, and the gap must be caught here: once the tip advances past it,
    /// the startup check can no longer tell.
    ///
    /// The response is to condemn, not to repair: clearing the stamp is one
    /// command, while purging is thousands of round-trips and this runs on the
    /// block-production path.
    #[tokio::test(flavor = "multi_thread")]
    async fn continuity_condemns_a_cache_that_missed_batches() {
        let (redis_db, postgres_db, _pg, _redis) = start_cache().await;
        let deployment_id = read_deployment_id(&postgres_db).await.unwrap();
        stamp_deployment_id(&redis_db, &deployment_id)
            .await
            .unwrap();

        let stale_pubkey = Pubkey::new_unique();
        let cached_tip = 100u64;
        let mut conn = redis_db.connection.clone();
        let _: () = conn.set("latest_slot", cached_tip).await.unwrap();
        let _: () = conn
            .set(format!("account:{}", stale_pubkey), vec![1u8, 2, 3])
            .await
            .unwrap();

        // Redis is back after missing slots 101..=200.
        assert!(
            ensure_cache_continuity(&redis_db, 201).await.unwrap(),
            "a missed batch must call for a rebuild"
        );

        let stamped: Option<Vec<u8>> = conn.get(DEPLOYMENT_ID_KEY).await.unwrap();
        assert_eq!(
            stamped, None,
            "the stamp must be cleared so read nodes stop trusting the cache"
        );
        let still_cached: bool = conn
            .exists(format!("account:{}", stale_pubkey))
            .await
            .unwrap();
        assert!(
            still_cached,
            "purging is left to the next startup, off the block-production path"
        );
    }

    /// An empty or freshly purged cache has no tip to be contiguous with, and
    /// nothing to lose either way.
    #[tokio::test(flavor = "multi_thread")]
    async fn continuity_accepts_a_cache_with_no_tip() {
        let (redis_db, postgres_db, _pg, _redis) = start_cache().await;
        let deployment_id = read_deployment_id(&postgres_db).await.unwrap();
        stamp_deployment_id(&redis_db, &deployment_id)
            .await
            .unwrap();

        assert!(
            !ensure_cache_continuity(&redis_db, 42).await.unwrap(),
            "a cache with no tip needs no rebuild"
        );

        let stamped: Option<Vec<u8>> = conn_get_stamp(&redis_db).await;
        assert_eq!(stamped, Some(deployment_id), "the stamp must be untouched");
    }

    async fn conn_get_stamp(redis_db: &RedisAccountsDB) -> Option<Vec<u8>> {
        let mut conn = redis_db.connection.clone();
        conn.get(DEPLOYMENT_ID_KEY).await.unwrap()
    }

    /// Rebuilding empties the cache and only then stamps it. The order is the
    /// point: a stamp written over a half-purged cache would put stale keys back
    /// into service.
    #[tokio::test(flavor = "multi_thread")]
    async fn rebuild_empties_the_cache_then_stamps_it() {
        let (redis_db, postgres_db, _pg, _redis) = start_cache().await;

        let stale_pubkey = Pubkey::new_unique();
        let mut conn = redis_db.connection.clone();
        let _: () = conn
            .set(format!("account:{}", stale_pubkey), vec![1u8, 2, 3])
            .await
            .unwrap();
        let _: () = conn.set("latest_slot", 100u64).await.unwrap();

        // Nothing has condemned the cache since, so this rebuild is the current
        // one and may stamp.
        rebuild_cache(&redis_db, redis_db.condemnation_generation())
            .await
            .unwrap();

        let stale_survived: bool = conn
            .exists(format!("account:{}", stale_pubkey))
            .await
            .unwrap();
        assert!(!stale_survived, "the rebuild must empty the cache");
        let tip: Option<u64> = conn.get("latest_slot").await.unwrap();
        assert_eq!(tip, None, "the rebuild must clear the cached tip too");
        let stamped: Option<Vec<u8>> = conn.get(DEPLOYMENT_ID_KEY).await.unwrap();
        assert_eq!(
            stamped,
            Some(read_deployment_id(&postgres_db).await.unwrap()),
            "a rebuilt cache must be stamped so reads can use it again"
        );
    }

    /// A rebuild that has been overtaken by a later condemnation must not stamp.
    ///
    /// With a flapping Redis two rebuilds can overlap, and the earlier one's purge
    /// predates the second gap: stamping on its completion would return keys
    /// written before that gap to service. Only the newest rebuild may stamp, and
    /// the ones it superseded finish quietly.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_superseded_rebuild_does_not_put_the_cache_back_in_service() {
        let (redis_db, _postgres_db, _pg, _redis) = start_cache().await;

        let stale_pubkey = Pubkey::new_unique();
        let mut conn = redis_db.connection.clone();
        let _: () = conn
            .set(format!("account:{}", stale_pubkey), vec![1u8, 2, 3])
            .await
            .unwrap();
        let _: () = conn.set("latest_slot", 100u64).await.unwrap();

        // Two gaps in a row: this rebuild carries the first generation while a
        // second condemnation has already happened.
        let superseded = redis_db.condemnation_generation();
        assert!(ensure_cache_continuity(&redis_db, 200).await.unwrap());
        let current = redis_db.condemnation_generation();
        assert_ne!(
            superseded, current,
            "each condemnation must advance the generation"
        );

        rebuild_cache(&redis_db, superseded).await.unwrap();

        let stamped: Option<Vec<u8>> = conn.get(DEPLOYMENT_ID_KEY).await.unwrap();
        assert_eq!(
            stamped, None,
            "a superseded rebuild must leave the cache out of service"
        );

        // The rebuild that carries the current generation is the one that may
        // finish the job.
        rebuild_cache(&redis_db, current).await.unwrap();
        let stamped: Option<Vec<u8>> = conn.get(DEPLOYMENT_ID_KEY).await.unwrap();
        assert!(
            stamped.is_some(),
            "the newest rebuild must put the cache back in service"
        );
    }

    /// A cache stamped for this deployment is the one a read node may serve.
    #[tokio::test(flavor = "multi_thread")]
    async fn verify_cache_stamp_accepts_a_matching_stamp() {
        let (redis_db, postgres_db, _pg, _redis) = start_cache().await;
        let deployment_id = read_deployment_id(&postgres_db).await.unwrap();
        stamp_deployment_id(&redis_db, &deployment_id)
            .await
            .unwrap();

        verify_cache_stamp(&redis_db)
            .await
            .expect("a cache stamped for this deployment must be accepted");
    }

    /// Another ledger's cache must never be served, which is the whole point of
    /// the stamp.
    #[tokio::test(flavor = "multi_thread")]
    async fn verify_cache_stamp_rejects_a_foreign_stamp() {
        let (redis_db, _postgres_db, _pg, _redis) = start_cache().await;
        stamp_deployment_id(&redis_db, &[9u8; 16]).await.unwrap();

        let error = verify_cache_stamp(&redis_db)
            .await
            .expect_err("a cache naming another deployment must be rejected");
        assert!(
            format!("{error}").contains("deployment"),
            "error should name the mismatch, got: {error}"
        );
    }

    /// An empty cache holds nothing to serve wrongly, so a read node may attach
    /// to it before any write node has stamped it.
    #[tokio::test(flavor = "multi_thread")]
    async fn verify_cache_stamp_accepts_an_empty_unstamped_cache() {
        let (redis_db, _postgres_db, _pg, _redis) = start_cache().await;

        verify_cache_stamp(&redis_db)
            .await
            .expect("an empty cache must be accepted");
    }

    /// Ledger state with no stamp is what a failed or interrupted purge leaves
    /// behind. It cannot be told apart from another ledger's data.
    #[tokio::test(flavor = "multi_thread")]
    async fn verify_cache_stamp_rejects_unstamped_ledger_state() {
        let (redis_db, _postgres_db, _pg, _redis) = start_cache().await;
        let mut conn = redis_db.connection.clone();
        let _: () = conn
            .set(format!("account:{}", Pubkey::new_unique()), vec![1u8, 2, 3])
            .await
            .unwrap();

        let error = verify_cache_stamp(&redis_db)
            .await
            .expect_err("unstamped ledger state must be rejected");
        assert!(
            format!("{error}").contains("deployment"),
            "error should name the missing stamp, got: {error}"
        );
    }
}
