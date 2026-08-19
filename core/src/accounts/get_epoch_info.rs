use {
    super::{postgres::PostgresAccountsDB, traits::AccountsDB},
    crate::rpc::api::EpochInfo,
    anyhow::{Context, Result},
};

// PrivateChannel doesn't have epochs like Solana - it has one massive epoch
// We use u64::MAX to represent an effectively infinite epoch
const SLOTS_IN_EPOCH: u64 = u64::MAX;
const EPOCH: u64 = 0;

pub async fn get_epoch_info(db: &AccountsDB) -> Result<EpochInfo> {
    match db {
        AccountsDB::Postgres(postgres_db) => get_epoch_info_postgres(postgres_db).await,
        // Served from the source of truth: it reports the transaction count,
        // which is not cached. See `get_transaction_count`.
        AccountsDB::Redis(redis_db) => get_epoch_info_postgres(&redis_db.fallback).await,
    }
}

async fn get_epoch_info_postgres(db: &PostgresAccountsDB) -> Result<EpochInfo> {
    let pool = db.pool.clone();

    // Get the latest slot
    let latest_slot = sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(slot) FROM blocks")
        .fetch_one(pool.as_ref())
        .await
        .context("Failed to query latest slot")?
        .context("No blocks found in database")? as u64;

    // Get transaction count (optional)
    let transaction_count = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT value FROM metadata WHERE key = 'transaction_count'",
    )
    .fetch_optional(pool.as_ref())
    .await
    .ok()
    .flatten()
    .and_then(|bytes| {
        super::transaction_count::TransactionCount::from_bytes(&bytes).map(|tc| tc.count())
    });

    Ok(EpochInfo {
        absolute_slot: latest_slot,
        block_height: latest_slot,
        epoch: EPOCH,
        slot_index: latest_slot,
        slots_in_epoch: SLOTS_IN_EPOCH,
        transaction_count,
    })
}
