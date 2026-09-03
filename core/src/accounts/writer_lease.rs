//! Single-writer gate for the write pipeline. A session advisory lock refuses a
//! second write-capable node at startup, and a heartbeat re-proves ownership so a
//! node that has silently lost the lock stops instead of running lease-less.

use {
    anyhow::{anyhow, Context, Result},
    sqlx::{Connection, PgConnection},
    std::time::Duration,
    tokio::task::JoinHandle,
    tokio_util::sync::CancellationToken,
    tracing::{error, info, warn},
};

/// Identifies the write-pipeline lease. Distinct from the truncation lock so an
/// admin truncation can still run alongside a live writer.
const WRITER_LEASE_LOCK_ID: i64 = 0x50435F_57524954; // "PC_WRIT" as hex

/// How often the lease session is asked to prove it still holds the lock.
pub const LEASE_PROBE_INTERVAL: Duration = Duration::from_secs(5);

/// Cap on one probe, so a hung backend is not read as a healthy one.
const LEASE_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Idle seconds before Postgres starts probing the lease socket, then the probe
/// spacing and how many may go unanswered. Same values as the gateway uses, and
/// together they reap a vanished writer in under two minutes.
const LEASE_KEEPALIVE_IDLE_SECS: u32 = 60;
const LEASE_KEEPALIVE_INTERVAL_SECS: u32 = 15;
const LEASE_KEEPALIVE_COUNT: u32 = 3;

/// Owns the Postgres session holding the writer lease, and the heartbeat task
/// that re-proves it. The connection is opened directly rather than pooled: sqlx
/// does not reset a returned connection, so a pooled lock would never free.
pub struct WriterLease {
    stop: CancellationToken,
    /// Taken by `release`, which is the only path that awaits the heartbeat.
    heartbeat: Option<JoinHandle<()>>,
}

/// Does this session still hold the lease?
///
/// Deliberately not `pg_try_advisory_lock`, which would silently retake a lost
/// lock and hide the gap. A bigint key lives in `classid` (high 32 bits) and
/// `objid` (low 32), so both are matched, along with our own backend pid.
async fn lock_is_still_held(conn: &mut PgConnection) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        r#"
        SELECT EXISTS (
          SELECT 1 FROM pg_locks
          WHERE locktype = 'advisory'
            AND pid = pg_backend_pid()
            AND objsubid = 1
            AND granted
            AND ((classid::bigint << 32) | objid::bigint) = $1
        )
        "#,
    )
    .bind(WRITER_LEASE_LOCK_ID)
    .fetch_one(conn)
    .await
}

/// Ask Postgres to reap this session quickly if the node's host disappears. A
/// vanished host sends no FIN, so the default leaves the lock held for about two
/// hours. Best effort: an unsupported platform or a unix socket ignores these.
async fn apply_lease_keepalives(conn: &mut PgConnection) {
    // `SET` takes no bind parameters, which would force the values into the
    // statement text.
    let applied = sqlx::query(
        "SELECT set_config('tcp_keepalives_idle', $1, false),
                set_config('tcp_keepalives_interval', $2, false),
                set_config('tcp_keepalives_count', $3, false)",
    )
    .bind(LEASE_KEEPALIVE_IDLE_SECS.to_string())
    .bind(LEASE_KEEPALIVE_INTERVAL_SECS.to_string())
    .bind(LEASE_KEEPALIVE_COUNT.to_string())
    .execute(conn)
    .await;

    if let Err(e) = applied {
        warn!(
            "Could not set TCP keepalives on the writer lease session: {}",
            e
        );
    }
}

/// Re-prove ownership on `interval`, and cancel `node_shutdown` the first time it
/// cannot be proven. No retries: during a full outage this node cannot do useful
/// work anyway, and during a partial one a replacement can already take the lock.
async fn run_heartbeat(
    mut conn: PgConnection,
    interval: Duration,
    stop: CancellationToken,
    node_shutdown: CancellationToken,
) {
    loop {
        // The sleep is raced, never the probe: dropping a query mid-flight would
        // leave the connection unusable.
        tokio::select! {
            biased;
            _ = stop.cancelled() => break,
            _ = tokio::time::sleep(interval) => {}
        }

        let verdict =
            tokio::time::timeout(LEASE_PROBE_TIMEOUT, lock_is_still_held(&mut conn)).await;
        let reason = match verdict {
            Ok(Ok(true)) => continue,
            Ok(Ok(false)) => "pg_locks reports the lease is no longer held",
            // A checked-out connection never heals, so an error means the session ended.
            Ok(Err(_)) => "the probe query failed on the lease session",
            Err(_) => "the probe did not answer within the timeout",
        };
        error!("Writer lease ownership could not be proven ({reason}); stopping the node");
        node_shutdown.cancel();

        // A false positive leaves the lock genuinely ours, so hold the socket open
        // until the node reports its workers stopped, then just drop it: a probe
        // that timed out was dropped mid-flight and cannot carry an unlock.
        stop.cancelled().await;
        return;
    }

    // Only a deliberate release reaches here, so the lock is ours to give back.
    if let Err(e) = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(WRITER_LEASE_LOCK_ID)
        .execute(&mut conn)
        .await
    {
        // Closing the session releases the lock anyway, so this is not fatal.
        warn!("Failed to release the writer lease explicitly: {}", e);
    }
    if let Err(e) = conn.close().await {
        warn!("Failed to close the writer lease connection: {}", e);
    }
    info!("Writer lease released");
}

impl WriterLease {
    /// Claim the lease, or fail if another write-capable node already holds it.
    /// Cancels `node_shutdown` if ownership later stops being provable.
    pub async fn acquire(database_url: &str, node_shutdown: CancellationToken) -> Result<Self> {
        Self::acquire_with_probe_interval(database_url, node_shutdown, LEASE_PROBE_INTERVAL).await
    }

    /// Same, with the probe interval chosen by the caller so a test can drive a
    /// lease loss without waiting out the production one.
    pub async fn acquire_with_probe_interval(
        database_url: &str,
        node_shutdown: CancellationToken,
        probe_interval: Duration,
    ) -> Result<Self> {
        let mut conn = PgConnection::connect(database_url)
            .await
            .context("Failed to open the writer lease connection")?;

        // Before the lock, so a session that takes it is already reapable.
        apply_lease_keepalives(&mut conn).await;

        let acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(WRITER_LEASE_LOCK_ID)
            .fetch_one(&mut conn)
            .await
            .context("Failed to acquire the writer lease")?;

        if !acquired {
            return Err(anyhow!(
                "Another write-capable node already holds the writer lease on this database. \
                 Only one write or aio node may run against a Postgres primary."
            ));
        }

        let stop = CancellationToken::new();
        let heartbeat = tokio::spawn(run_heartbeat(
            conn,
            probe_interval,
            stop.clone(),
            node_shutdown,
        ));

        info!("Writer lease acquired");
        Ok(Self {
            stop,
            heartbeat: Some(heartbeat),
        })
    }

    /// Give the lease up so a replacement node can start immediately.
    pub async fn release(mut self) {
        self.stop.cancel();
        if let Some(heartbeat) = self.heartbeat.take() {
            if let Err(e) = heartbeat.await {
                warn!("Writer lease heartbeat did not stop cleanly: {}", e);
            }
        }
    }

    /// Keep the lock until this process exits, for a shutdown that could not prove
    /// every worker had stopped. Releasing it there would let a replacement start
    /// while a detached worker can still commit.
    pub fn hold(self) {
        std::mem::forget(self);
    }
}

impl Drop for WriterLease {
    /// A lease dropped on an error path still has to free the lock, so cancel and
    /// let the heartbeat close the session on its way out.
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{postgres_container_url, start_test_postgres_with_url};

    /// Kill every other backend on this database, which is what a failover, a
    /// connection reaper or `pg_terminate_backend` does to the lease session.
    async fn terminate_other_backends(pool: &sqlx::PgPool) {
        sqlx::query(
            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity
             WHERE datname = current_database() AND pid <> pg_backend_pid()",
        )
        .execute(pool)
        .await
        .expect("failed to terminate the lease backend");
    }

    /// One holder at a time, and the lease must come back after a release, which
    /// is what lets a restarted node take over from the one it replaces.
    #[tokio::test(flavor = "multi_thread")]
    async fn writer_lease_is_exclusive_and_reusable_after_release() {
        let (_db, _pg, url) = start_test_postgres_with_url().await;
        let shutdown = CancellationToken::new();

        let first = WriterLease::acquire(&url, shutdown.clone())
            .await
            .expect("the first lease must be granted");

        let err = WriterLease::acquire(&url, shutdown.clone())
            .await
            .err()
            .expect("a second lease on the same database must be refused");
        assert!(
            err.to_string().contains("writer lease"),
            "the error must name the writer lease, got: {err}"
        );

        first.release().await;
        assert!(
            !shutdown.is_cancelled(),
            "a deliberate release must not look like a lost lease"
        );

        WriterLease::acquire(&url, shutdown)
            .await
            .expect("the lease must be available again after a release");
    }

    /// A failed startup drops the lease instead of releasing it, and that must
    /// still free the lock, or the next write node in this process is refused.
    #[tokio::test(flavor = "multi_thread")]
    async fn writer_lease_dropped_without_release_still_frees_the_lock() {
        let (_db, _pg, url) = start_test_postgres_with_url().await;
        let shutdown = CancellationToken::new();

        drop(
            WriterLease::acquire(&url, shutdown.clone())
                .await
                .expect("the first lease must be granted"),
        );

        // The unlock runs on the heartbeat task, so it lands a moment later.
        for _ in 0..50 {
            if WriterLease::acquire(&url, shutdown.clone()).await.is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("a dropped lease must free the lock");
    }

    /// Advisory locks are per database, so two deployments sharing one Postgres
    /// server do not lock each other out.
    #[tokio::test(flavor = "multi_thread")]
    async fn writer_lease_does_not_span_databases() {
        let (db, pg, url) = start_test_postgres_with_url().await;
        let shutdown = CancellationToken::new();

        sqlx::query("CREATE DATABASE other_deployment")
            .execute(db.pool.as_ref())
            .await
            .expect("failed to create the second database");
        let other_url = postgres_container_url(&pg, "other_deployment").await;

        let _held = WriterLease::acquire(&url, shutdown.clone())
            .await
            .expect("the first lease must be granted");

        WriterLease::acquire(&other_url, shutdown)
            .await
            .expect("a lease on a different database must not be blocked");
    }

    /// A lease can be lost without the node noticing: a failover, a proxy reaper
    /// or a terminated backend all drop the lock while the node keeps writing.
    /// The node must stop, or it runs on lease-less while a replacement starts.
    #[tokio::test(flavor = "multi_thread")]
    async fn writer_lease_stops_the_node_when_its_lock_is_lost() {
        let (db, _pg, url) = start_test_postgres_with_url().await;
        let shutdown = CancellationToken::new();

        let _lease = WriterLease::acquire_with_probe_interval(
            &url,
            shutdown.clone(),
            Duration::from_millis(50),
        )
        .await
        .expect("the lease must be granted");

        terminate_other_backends(db.pool.as_ref()).await;

        tokio::time::timeout(Duration::from_secs(10), shutdown.cancelled())
            .await
            .expect("losing the lease must stop the node");
    }

    /// Can a third session take `id` right now? Unlocks again so the answer is
    /// repeatable on a pooled connection, which sqlx never resets.
    async fn lock_is_free(pool: &sqlx::PgPool, id: i64) -> bool {
        let mut conn = pool.acquire().await.expect("failed to check the lock");
        let taken: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(id)
            .fetch_one(&mut *conn)
            .await
            .expect("failed to probe the lock");
        if taken {
            sqlx::query("SELECT pg_advisory_unlock($1)")
                .bind(id)
                .execute(&mut *conn)
                .await
                .expect("failed to release the probe lock");
        }
        taken
    }

    /// A verdict can be a false positive, and the node keeps committing for the
    /// whole drain. So an unprovable probe must stop the node without ending the
    /// session: the lock lives exactly as long as that session does.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_unprovable_probe_keeps_the_lease_session_until_it_is_stopped() {
        /// Stands in for the lease lock. Held on the same session, so it is held
        /// for exactly as long, and a third session can watch it from outside.
        const WITNESS_LOCK_ID: i64 = 0x50435F_5749544E; // "PC_WITN" as hex

        let (db, _pg, url) = start_test_postgres_with_url().await;
        let mut conn = PgConnection::connect(&url)
            .await
            .expect("failed to open the lease connection");
        let taken: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(WITNESS_LOCK_ID)
            .fetch_one(&mut conn)
            .await
            .expect("failed to take the witness lock");
        assert!(taken, "the witness lock must start free");

        // This session never took the lease, so the first probe proves "not held"
        // at once and no healthy probe has to be made to lie.
        let stop = CancellationToken::new();
        let node_shutdown = CancellationToken::new();
        let heartbeat = tokio::spawn(run_heartbeat(
            conn,
            Duration::from_millis(50),
            stop.clone(),
            node_shutdown.clone(),
        ));

        tokio::time::timeout(Duration::from_secs(10), node_shutdown.cancelled())
            .await
            .expect("an unprovable probe must stop the node");

        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !heartbeat.is_finished(),
            "the heartbeat must outlive the verdict, or the lock goes with it"
        );
        assert!(
            !lock_is_free(db.pool.as_ref(), WITNESS_LOCK_ID).await,
            "the lease session must still be open while the node drains"
        );

        stop.cancel();
        tokio::time::timeout(Duration::from_secs(5), heartbeat)
            .await
            .expect("stopping the lease must end the heartbeat")
            .expect("the heartbeat must not panic");

        // The socket closes as the connection drops, so the lock frees a moment
        // later rather than on the await above.
        for _ in 0..50 {
            if lock_is_free(db.pool.as_ref(), WITNESS_LOCK_ID).await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("stopping the lease must close its session");
    }

    /// A host that vanishes sends no FIN, so without these the lease backend sits
    /// in recv() holding the lock until the OS default expires, about two hours.
    /// Postgres reports 0 over a unix socket; the test container speaks TCP.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_lease_session_sets_its_own_tcp_keepalives() {
        let (_db, _pg, url) = start_test_postgres_with_url().await;
        let mut conn = PgConnection::connect(&url)
            .await
            .expect("failed to open the lease connection");

        apply_lease_keepalives(&mut conn).await;

        let settings: Vec<(String, String)> = sqlx::query_as(
            "SELECT name, setting FROM pg_settings
             WHERE name LIKE 'tcp_keepalives%' ORDER BY name",
        )
        .fetch_all(&mut conn)
        .await
        .expect("failed to read the keepalive settings");

        assert_eq!(
            settings,
            vec![
                (
                    "tcp_keepalives_count".to_string(),
                    LEASE_KEEPALIVE_COUNT.to_string()
                ),
                (
                    "tcp_keepalives_idle".to_string(),
                    LEASE_KEEPALIVE_IDLE_SECS.to_string()
                ),
                (
                    "tcp_keepalives_interval".to_string(),
                    LEASE_KEEPALIVE_INTERVAL_SECS.to_string()
                ),
            ],
            "the lease session must carry its own keepalives"
        );
    }
}
