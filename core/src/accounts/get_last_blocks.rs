use {
    super::{
        postgres::PostgresAccountsDB,
        traits::{AccountsDB, BlockInfo},
    },
    anyhow::{bail, ensure, Context, Result},
    sqlx::Row,
};

/// The newest `limit` blocks, oldest first. A count of blocks, not a span of
/// slots: the blockhash window is `max_blockhashes` blocks, and a slot range that
/// wide holds far fewer of them once idle ticks stop producing one each.
///
/// Fails rather than returning a window with blocks missing from the middle or
/// the top, because the dedup rebuild reads this and a dropped block there leaves
/// its transactions replayable.
pub async fn get_last_blocks(db: &AccountsDB, limit: usize) -> Result<Vec<BlockInfo>> {
    match db {
        AccountsDB::Postgres(postgres_db) => get_last_blocks_postgres(postgres_db, limit).await,
        // Served from the source of truth: the cache cannot express which blocks
        // it is missing, and this path feeds the dedup rebuild, where a dropped
        // block means a replay slips through.
        AccountsDB::Redis(redis_db) => get_last_blocks_postgres(&redis_db.fallback, limit).await,
    }
}

async fn get_last_blocks_postgres(db: &PostgresAccountsDB, limit: usize) -> Result<Vec<BlockInfo>> {
    let pool = db.pool.clone();

    // The rows and the counter are compared below, so they are read in one
    // snapshot. Read committed takes a fresh snapshot per statement, which would
    // straddle a concurrent block and report a healthy ledger as truncated.
    let mut tx = pool
        .begin()
        .await
        .context("Failed to open the recent-blocks read transaction")?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *tx)
        .await
        .context("Failed to pin the recent-blocks read to one snapshot")?;

    let rows = sqlx::query("SELECT data FROM blocks ORDER BY slot DESC LIMIT $1")
        .bind(limit as i64)
        .fetch_all(&mut *tx)
        .await
        .context("Failed to query the most recent blocks")?;

    // The raw counter, not the height reader: that one falls back to the highest
    // slot when the key is gone, which would compare a slot against a height.
    let durable_height = super::get_block_height::read_block_height_counter(&mut *tx).await?;

    tx.commit()
        .await
        .context("Failed to close the recent-blocks read transaction")?;

    let mut blocks = Vec::with_capacity(rows.len());
    for row in rows.into_iter().rev() {
        let data: Vec<u8> = row.get("data");
        // This path feeds the dedup rebuild, so a decode failure fails closed
        // rather than silently seeding a short cache.
        let block = bincode::deserialize::<BlockInfo>(&data)
            .context("Failed to deserialize a recent block (likely pre-upgrade block data; wipe the DB or add a migration shim)")?;
        blocks.push(block);
    }

    verify_dedup_window(&blocks, durable_height)?;

    Ok(blocks)
}

/// Prove the restored blocks are the unbroken tail of the chain. Heights are
/// checked rather than slots, because the writer bumps the height once per stored
/// block while idle ticks advance the slot without storing anything.
fn verify_dedup_window(blocks: &[BlockInfo], durable_height: Option<u64>) -> Result<()> {
    if blocks.is_empty() {
        // An empty table is a fresh ledger only when no counter claims otherwise.
        if let Some(durable) = durable_height {
            bail!(
                "the ledger has no block rows but the durable block height counter is {durable}, \
                 so every block in the dedup window is missing; restore from backup"
            );
        }
        return Ok(());
    }

    // Only the oldest blocks may be absent: their transactions can only name a
    // blockhash that went with them, so nothing they held is still replayable.
    let mut previous: Option<(u64, u64)> = None;
    for block in blocks {
        let height = block.block_height.ok_or_else(|| {
            anyhow::anyhow!(
                "the block at slot {} has no block height, so the dedup window cannot be proven \
                 complete",
                block.slot
            )
        })?;

        if let Some((previous_slot, previous_height)) = previous {
            ensure!(
                Some(height) == previous_height.checked_add(1),
                "the dedup restore window has a gap: the block at slot {} has height {} but the \
                 block before it at slot {} has height {}; restore from backup",
                block.slot,
                height,
                previous_slot,
                previous_height,
            );
        }
        previous = Some((block.slot, height));
    }

    let (newest_slot, newest_height) = previous.expect("the loop ran over a non-empty slice");
    match durable_height {
        Some(durable) => ensure!(
            newest_height == durable,
            "the newest restored block at slot {newest_slot} has height {newest_height} but the \
             durable block height counter is {durable}, so the dedup window is not the chain tip; \
             restore from backup",
        ),
        None => bail!(
            "the durable block height counter is missing while the blocks table still holds rows, \
             so the dedup window cannot be proven complete; restore from backup"
        ),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use {super::*, crate::test_helpers::create_test_block_info, solana_sdk::hash::Hash};

    fn block(slot: u64, height: Option<u64>) -> BlockInfo {
        let mut block = create_test_block_info(slot, Hash::new_unique());
        block.block_height = height;
        block
    }

    /// A case name, the (slot, height) rows to check, the durable counter, and
    /// the fragments the error must name. No fragments means the window passes.
    type WindowCase = (
        &'static str,
        &'static [(u64, Option<u64>)],
        Option<u64>,
        &'static [&'static str],
    );

    /// One table over every window shape the restore can see. The expected
    /// fragments are asserted so an operator can tell which blocks went missing.
    #[test]
    fn verify_dedup_window_accepts_only_an_unbroken_tail() {
        let cases: &[WindowCase] = &[
            ("fresh ledger", &[], None, &[]),
            (
                "rows gone under a live tip",
                &[],
                Some(0),
                &["no block rows"],
            ),
            ("genesis only", &[(0, Some(0))], Some(0), &[]),
            (
                "sparse slots, contiguous heights",
                &[(0, Some(0)), (10, Some(1)), (20, Some(2)), (25, Some(3))],
                Some(3),
                &[],
            ),
            (
                "truncated prefix",
                &[(50, Some(5)), (60, Some(6)), (70, Some(7))],
                Some(7),
                &[],
            ),
            (
                "interior gap",
                &[(0, Some(0)), (10, Some(1)), (30, Some(3))],
                Some(3),
                &["height 3", "height 1"],
            ),
            (
                "newest row missing",
                &[(0, Some(0)), (10, Some(1)), (20, Some(2))],
                Some(3),
                &["counter is 3"],
            ),
            (
                "counter behind the tip",
                &[(0, Some(0)), (10, Some(1)), (20, Some(2))],
                Some(1),
                &["counter is 1"],
            ),
            (
                "counter missing with rows",
                &[(0, Some(0))],
                None,
                &["counter is missing"],
            ),
            (
                "height absent",
                &[(0, Some(0)), (10, None)],
                Some(1),
                &["no block height"],
            ),
        ];

        for (name, rows, durable, expected) in cases {
            let blocks: Vec<BlockInfo> = rows.iter().map(|(s, h)| block(*s, *h)).collect();
            let result = verify_dedup_window(&blocks, *durable);

            match result {
                Ok(()) => assert!(
                    expected.is_empty(),
                    "{name}: the window must be rejected, but it passed"
                ),
                Err(e) => {
                    assert!(
                        !expected.is_empty(),
                        "{name}: the window must pass, but it was rejected with {e}"
                    );
                    let message = e.to_string();
                    for fragment in *expected {
                        assert!(
                            message.contains(fragment),
                            "{name}: the error must name {fragment:?}, got {message:?}"
                        );
                    }
                }
            }
        }
    }
}
