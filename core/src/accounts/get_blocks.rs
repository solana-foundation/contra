use {
    super::{postgres::PostgresAccountsDB, redis::RedisAccountsDB, traits::AccountsDB},
    anyhow::{anyhow, Context, Result},
    redis::AsyncCommands,
};

/// Maximum number of blocks that can be returned (per Solana spec)
const MAX_BLOCKS_RANGE: u64 = 500_000;

pub async fn get_blocks(
    db: &AccountsDB,
    start_slot: u64,
    end_slot: Option<u64>,
) -> Result<Vec<u64>> {
    match db {
        AccountsDB::Postgres(postgres_db) => {
            get_blocks_postgres(postgres_db, start_slot, end_slot).await
        }
        AccountsDB::Redis(redis_db) => get_blocks_redis(redis_db, start_slot, end_slot).await,
    }
}

async fn get_blocks_postgres(
    db: &PostgresAccountsDB,
    start_slot: u64,
    end_slot: Option<u64>,
) -> Result<Vec<u64>> {
    let pool = db.pool.clone();

    let end_slot = match end_slot {
        Some(end) => end,
        None => sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(slot) FROM blocks")
            .fetch_one(pool.as_ref())
            .await
            .context("Failed to query latest slot")?
            .context("No blocks found in database")? as u64,
    };

    // Enforce maximum range constraint
    if end_slot > start_slot && (end_slot - start_slot) > MAX_BLOCKS_RANGE {
        return Err(anyhow!(
            "Range too large: {} slots (max: {})",
            end_slot - start_slot,
            MAX_BLOCKS_RANGE
        ));
    }

    // Query blocks within the range
    let slots = sqlx::query_scalar::<_, i64>(
        "SELECT slot FROM blocks WHERE slot >= $1 AND slot <= $2 ORDER BY slot ASC",
    )
    .bind(start_slot as i64)
    .bind(end_slot as i64)
    .fetch_all(pool.as_ref())
    .await
    .context("Failed to query blocks")?;

    // Convert i64 slots to u64
    Ok(slots.into_iter().map(|s| s as u64).collect())
}

async fn get_blocks_redis(
    db: &RedisAccountsDB,
    start_slot: u64,
    end_slot: Option<u64>,
) -> Result<Vec<u64>> {
    let mut conn = db.connection.clone();

    let end_slot = match end_slot {
        Some(end) => end,
        None => {
            let latest_slot: redis::RedisResult<Option<u64>> = conn.get("latest_slot").await;
            latest_slot
                .map_err(|e| anyhow!("Failed to get latest slot from Redis: {}", e))?
                .context("No latest slot found in Redis")?
        }
    };

    // Enforce maximum range constraint
    if end_slot > start_slot && (end_slot - start_slot) > MAX_BLOCKS_RANGE {
        return Err(anyhow!(
            "Range too large: {} slots (max: {})",
            end_slot - start_slot,
            MAX_BLOCKS_RANGE
        ));
    }

    // Use SCAN with a pattern to avoid O(range) round trips.
    // Checking each slot individually via EXISTS would require up to MAX_BLOCKS_RANGE
    // (500,000) sequential round trips — far too slow and connection-breaking.
    let mut cursor = 0u64;
    let mut slots = Vec::new();
    loop {
        let (new_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
            .arg(cursor)
            .arg("MATCH")
            .arg("block:*")
            .arg("COUNT")
            .arg(100)
            .query_async(&mut conn)
            .await
            .map_err(|e| anyhow!("Failed to scan blocks in Redis: {}", e))?;

        for key in keys {
            if let Some(slot_str) = key.strip_prefix("block:") {
                if let Ok(slot) = slot_str.parse::<u64>() {
                    if slot >= start_slot && slot <= end_slot {
                        slots.push(slot);
                    }
                }
            }
        }

        cursor = new_cursor;
        if cursor == 0 {
            break;
        }
    }

    slots.sort_unstable();
    Ok(slots)
}
