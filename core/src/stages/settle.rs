use {
    crate::{
        accounts::{
            postgres::PostgresAccountsDB, redis::RedisAccountsDB, traits::BlockInfo, AccountsDB,
        },
        nodes::node::WorkerHandle,
    },
    anyhow::{anyhow, Context, Result},
    redis::AsyncCommands,
    solana_hash::Hash,
    solana_rpc_client_types::response::RpcPerfSample,
    solana_sdk::{
        account::{AccountSharedData, ReadableAccount},
        pubkey::Pubkey,
        transaction::SanitizedTransaction,
    },
    solana_svm::{
        transaction_processing_result::{ProcessedTransaction, TransactionProcessingResult},
        transaction_processor::LoadAndExecuteSanitizedTransactionsOutput,
    },
    solana_svm_transaction::svm_message::SVMMessage,
    std::{
        collections::HashMap,
        time::{Duration, SystemTime, UNIX_EPOCH},
    },
    tokio::{sync::mpsc, time::Instant},
    tokio_util::sync::CancellationToken,
    tracing::{debug, error, info, warn},
};

const SETTLE_START_DELAY_MS: u64 = 1000;

/// A single account that has been settled
/// We need to track if the account was deleted so we can tombstone it
/// in the accounts database
pub struct AccountSettlement {
    pub account: AccountSharedData,
    pub deleted: bool,
}

struct SettleResult {
    slot: u64,
    blockhash: Hash,
    account_settlements: Vec<(Pubkey, AccountSettlement)>,
}

#[derive(Clone)]
struct LastBlock {
    slot: u64,
    blockhash: Hash,
}

/// Warm the Redis cache by reading from Postgres and writing to Redis
/// This is called on startup to ensure Redis has the latest state from Postgres
pub async fn warm_redis_cache(
    postgres_db: &PostgresAccountsDB,
    redis_db: &RedisAccountsDB,
) -> Result<()> {
    info!("Warming Redis cache from Postgres...");

    // Read latest_slot from Postgres
    let pool = postgres_db.pool.clone();
    let slot = sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(slot) FROM blocks")
        .fetch_one(pool.as_ref())
        .await
        .context("Failed to query latest slot from Postgres")?;

    if let Some(slot_value) = slot {
        let slot_u64 = slot_value as u64;

        // Write latest_slot to Redis
        let mut conn = redis_db.connection.clone();
        conn.set::<_, _, ()>("latest_slot", slot_u64)
            .await
            .map_err(|e| anyhow!("Failed to write latest_slot to Redis: {}", e))?;

        info!("Warmed Redis cache: latest_slot = {}", slot_u64);
    } else {
        warn!("No blocks found in Postgres - skipping latest_slot cache warming");
    }

    // Read latest_blockhash from Postgres
    let blockhash_bytes: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT value FROM metadata WHERE key = 'latest_blockhash'")
            .fetch_optional(pool.as_ref())
            .await
            .context("Failed to query latest blockhash from Postgres")?;

    if let Some(bytes) = blockhash_bytes {
        // Convert bytes to Hash and then to string for Redis storage
        let hash_array: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("Invalid blockhash bytes length: {}", bytes.len()))?;
        let hash = Hash::new_from_array(hash_array);
        let hash_str = hash.to_string();

        // Write latest_blockhash to Redis
        let mut conn = redis_db.connection.clone();
        conn.set::<_, _, ()>("latest_blockhash", hash_str.clone())
            .await
            .map_err(|e| anyhow!("Failed to write latest_blockhash to Redis: {}", e))?;

        info!("Warmed Redis cache: latest_blockhash = {}", hash_str);
    } else {
        warn!("No blockhash found in Postgres metadata - skipping latest_blockhash cache warming");
    }

    info!("Redis cache warming completed successfully");
    Ok(())
}

pub struct SettleArgs {
    pub execution_results_rx: mpsc::UnboundedReceiver<(
        LoadAndExecuteSanitizedTransactionsOutput,
        Vec<SanitizedTransaction>,
    )>,
    pub settled_accounts_tx: mpsc::UnboundedSender<Vec<(Pubkey, AccountSettlement)>>,
    pub settled_blockhashes_tx: mpsc::UnboundedSender<Hash>,
    pub accountsdb_connection_url: String,
    pub blocktime_ms: u64,
    pub perf_sample_period_secs: u64,
    pub shutdown_token: CancellationToken,
}

pub async fn start_settle_worker(args: SettleArgs) -> WorkerHandle {
    let SettleArgs {
        execution_results_rx,
        settled_accounts_tx,
        settled_blockhashes_tx,
        accountsdb_connection_url,
        blocktime_ms,
        perf_sample_period_secs,
        shutdown_token,
    } = args;
    let handle = tokio::spawn(async move {
        async fn run_settle_worker(
            mut execution_results_rx: mpsc::UnboundedReceiver<(
                LoadAndExecuteSanitizedTransactionsOutput,
                Vec<SanitizedTransaction>,
            )>,
            settled_accounts_tx: mpsc::UnboundedSender<Vec<(Pubkey, AccountSettlement)>>,
            settled_blockhashes_tx: mpsc::UnboundedSender<Hash>,
            accountsdb_connection_url: String,
            blocktime_ms: u64,
            perf_sample_period_secs: u64,
            shutdown_token: CancellationToken,
        ) -> anyhow::Result<()> {
            info!("Settle worker started");

            let mut accounts_db = AccountsDB::new(&accountsdb_connection_url, false)
                .await
                .unwrap();

            let mut redis_db: Option<RedisAccountsDB> = match std::env::var("REDIS_URL") {
                Ok(redis_url) => {
                    match tokio::time::timeout(
                        Duration::from_secs(5),
                        RedisAccountsDB::new(&redis_url),
                    )
                    .await
                    {
                        Ok(Ok(r)) => {
                            info!("Redis cache enabled");
                            Some(r)
                        }
                        Ok(Err(e)) => {
                            warn!("Redis unavailable ({}), running Postgres-only", e);
                            None
                        }
                        Err(_) => {
                            warn!("Redis connection timed out, running Postgres-only");
                            None
                        }
                    }
                }
                Err(_) => {
                    info!("REDIS_URL not set, running Postgres-only");
                    None
                }
            };

            // Warm Redis cache from Postgres on startup
            if let (AccountsDB::Postgres(ref pg), Some(ref redis)) = (&accounts_db, &redis_db) {
                if let Err(e) = warm_redis_cache(pg, redis).await {
                    warn!("Cache warming failed (non-fatal): {}", e);
                }
            }

            let last_slot = accounts_db.get_latest_slot().await.ok().flatten();
            let last_blockhash = accounts_db.get_latest_blockhash().await.ok();

            // Validate that last_slot and last_blockhash are both present or both absent
            match (last_slot, last_blockhash) {
                (Some(_), None) => {
                    anyhow::bail!("Invalid state: last_slot exists but last_blockhash is missing");
                }
                (None, Some(_)) => {
                    anyhow::bail!("Invalid state: last_blockhash exists but last_slot is missing");
                }
                _ => {}
            }

            let mut last_block = match (last_slot, last_blockhash) {
                (Some(last_slot), Some(last_blockhash)) => Some(LastBlock {
                    slot: last_slot,
                    blockhash: last_blockhash,
                }),
                _ => None,
            };
            let mut processing_results = Vec::new();
            let mut blocktime_interval = tokio::time::interval_at(
                Instant::now() + Duration::from_millis(SETTLE_START_DELAY_MS),
                Duration::from_millis(blocktime_ms),
            );

            // Performance sample tracking
            let mut perf_sample_interval = tokio::time::interval_at(
                Instant::now() + Duration::from_secs(perf_sample_period_secs),
                Duration::from_secs(perf_sample_period_secs),
            );
            let mut perf_start_slot = last_block.as_ref().map(|b| b.slot).unwrap_or(0);
            let mut perf_num_transactions = 0u64;

            loop {
                tokio::select! {
                    // Settle transactions every BLOCKTIME_MS
                    _ = blocktime_interval.tick() => {
                        if let Ok(settle_result) = settle_transactions(last_block.clone(), &mut accounts_db, redis_db.as_mut(), &processing_results).await {
                            // Track performance metrics
                            let num_txs = processing_results.len() as u64;
                            perf_num_transactions += num_txs;

                            last_block = Some(LastBlock {
                                slot: settle_result.slot,
                                blockhash: settle_result.blockhash,
                            });
                            processing_results.clear();
                            debug!("Settled {} transactions in slot {}, blockhash {}", settle_result.account_settlements.len(), settle_result.slot, settle_result.blockhash);
                            if let Err(e) = settled_accounts_tx.send(settle_result.account_settlements) {
                                warn!("Failed to send settled accounts: {:?}", e);
                                break;
                            }
                            if let Err(e) = settled_blockhashes_tx.send(settle_result.blockhash) {
                                warn!("Failed to send settled blockhashes: {:?}", e);
                                break;
                            }
                        } else {
                            error!("Failed to settle transactions");
                            break;
                        }
                    }

                    // Save performance sample periodically
                    _ = perf_sample_interval.tick() => {
                        if let Some(ref current_block) = last_block {
                            let current_slot = current_block.slot;
                            let num_slots = current_slot.saturating_sub(perf_start_slot);

                            let sample = RpcPerfSample {
                                slot: current_slot,
                                num_transactions: perf_num_transactions,
                                num_slots,
                                sample_period_secs: perf_sample_period_secs as u16,
                                // In Contra, all transactions are non-vote transactions
                                num_non_vote_transactions: Some(perf_num_transactions),
                            };

                            if let Err(e) = accounts_db.store_performance_sample(sample).await {
                                warn!("Failed to store performance sample: {:?}", e);
                            } else {
                                debug!("Stored performance sample for slot {}: {} txs over {} slots",
                                    current_slot, perf_num_transactions, num_slots);
                            }

                            // Reset counters for next period
                            perf_start_slot = current_slot;
                            perf_num_transactions = 0;
                        }
                    }

                    // Process execution results
                    result = execution_results_rx.recv() => {
                        match result {
                            Some((svm_output, transactions)) => {
                                debug!("Settle worker received output with {} transactions", transactions.len());
                                if svm_output.processing_results.len() != transactions.len() {
                                    error!("Processing results and transactions length mismatch");
                                    break;
                                }
                                info!("Extending {} processing results", svm_output.processing_results.len());
                                processing_results.extend(svm_output.processing_results.into_iter().zip(transactions.into_iter()));
                            }
                            None => {
                                info!("Settle worker stopped - channel closed");
                                break;
                            }
                        }
                    }

                    // Handle shutdown signal
                    _ = shutdown_token.cancelled() => {
                        info!("Settle worker received shutdown signal");
                        break;
                    }
                }
            }

            info!("Settle worker stopped");
            Ok(())
        }

        if let Err(e) = run_settle_worker(
            execution_results_rx,
            settled_accounts_tx,
            settled_blockhashes_tx,
            accountsdb_connection_url,
            blocktime_ms,
            perf_sample_period_secs,
            shutdown_token,
        )
        .await
        {
            error!("Settle worker failed: {:?}", e);
        }
    });

    WorkerHandle::new("Settle".to_string(), handle)
}

/// Settle transactions: Update accounts database with changes
async fn settle_transactions(
    last_block: Option<LastBlock>,
    accounts_db: &mut AccountsDB,
    redis_db: Option<&mut RedisAccountsDB>,
    processing_results: &[(TransactionProcessingResult, SanitizedTransaction)],
) -> Result<SettleResult, Box<dyn std::error::Error>> {
    let mut final_accounts_actual: HashMap<Pubkey, AccountSettlement> = HashMap::new();

    // Determine block time
    let block_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    // Generate blockhash and determine next slot
    // TODO: Check the blockhash generation scheme
    let (next_blockhash, next_slot, last_blockhash, last_slot) =
        if let Some(ref last_block) = last_block {
            let mut hash_bytes = [0u8; 32];
            hash_bytes[0..8].copy_from_slice(&last_block.slot.to_le_bytes());
            hash_bytes[8..16].copy_from_slice(&block_time.to_le_bytes());
            let next_blockhash = Hash::new_from_array(hash_bytes);
            let next_slot = last_block.slot + 1;
            (
                next_blockhash,
                next_slot,
                last_block.blockhash,
                last_block.slot,
            )
        } else {
            (Hash::default(), 0, Hash::default(), 0)
        };

    // Start collecting transaction signatures for this block
    let mut block_transaction_signatures = Vec::new();
    let mut block_transaction_recent_blockhashes = Vec::new();
    let mut transactions_for_db = Vec::new();

    for (processing_result, sanitized_transaction) in processing_results.iter() {
        let signature = sanitized_transaction.signature();
        let recent_blockhash = *sanitized_transaction.message().recent_blockhash();

        // Only collect successful transactions for batch write
        if let Ok(processed_tx) = processing_result {
            transactions_for_db.push((
                *signature,
                sanitized_transaction,
                next_slot,
                block_time,
                processed_tx,
            ));
        }

        match processing_result {
            Ok(ProcessedTransaction::Executed(executed_tx)) => {
                debug!(
                    "Executed transaction: {:?}",
                    sanitized_transaction.signature()
                );

                for (index, (pubkey, account_data)) in
                    executed_tx.loaded_transaction.accounts.iter().enumerate()
                {
                    if sanitized_transaction.is_writable(index) {
                        if account_data.lamports() == 0 && account_data.data().is_empty() {
                            final_accounts_actual.insert(
                                *pubkey,
                                AccountSettlement {
                                    account: account_data.clone(),
                                    deleted: true,
                                },
                            );
                        } else {
                            final_accounts_actual.insert(
                                *pubkey,
                                AccountSettlement {
                                    account: account_data.clone(),
                                    deleted: false,
                                },
                            );
                        }
                    }
                }

                block_transaction_signatures.push(*signature);
                block_transaction_recent_blockhashes.push(recent_blockhash);
            }
            Ok(ProcessedTransaction::FeesOnly(fees_only_transaction)) => {
                warn!("FeesOnly transaction: {:?}", fees_only_transaction);

                // For fees-only transactions, we just record the transaction
                // The rollback accounts have already been handled by SVM
                // and fees have been deducted

                block_transaction_signatures.push(*signature);
                block_transaction_recent_blockhashes.push(recent_blockhash);
            }
            Err(e) => {
                warn!("Transaction failed: {:?}, error: {:?}", signature, e);
                // Failed transactions still get recorded
                block_transaction_signatures.push(*signature);
                block_transaction_recent_blockhashes.push(recent_blockhash);
            }
        }
    }

    // Convert final_accounts to Vec for batch write
    let accounts_vec: Vec<(Pubkey, AccountSettlement)> =
        final_accounts_actual.into_iter().collect();

    // Create block info
    let block_info = BlockInfo {
        slot: next_slot,
        blockhash: next_blockhash,
        previous_blockhash: last_blockhash,
        parent_slot: last_slot,
        // TODO: Do we need this?
        block_height: Some(next_slot),
        block_time: Some(block_time),
        transaction_signatures: block_transaction_signatures,
        transaction_recent_blockhashes: block_transaction_recent_blockhashes,
    };

    // Write to Postgres (source of truth, fatal on failure)
    accounts_db
        .write_batch(
            &accounts_vec,
            transactions_for_db.clone(),
            Some(block_info.clone()),
        )
        .await?;

    // Write to Redis best-effort (non-fatal)
    if let Some(redis) = redis_db {
        if let Err(e) = crate::accounts::write_batch::write_batch_redis(
            redis,
            &accounts_vec,
            transactions_for_db,
            Some(block_info),
        )
        .await
        {
            warn!(
                "Best-effort Redis cache write failed (non-fatal, Postgres succeeded): {}",
                e
            );
        }
    }

    Ok(SettleResult {
        slot: next_slot,
        blockhash: next_blockhash,
        account_settlements: accounts_vec,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use redis::AsyncCommands;
    use solana_hash::Hash;

    /// Test that cache warming reads from Postgres and writes to Redis correctly.
    ///
    /// This test verifies:
    /// 1. Reads latest_slot from Postgres (MAX(slot) from blocks table)
    /// 2. Writes latest_slot to Redis
    /// 3. Reads latest_blockhash from Postgres metadata table
    /// 4. Writes latest_blockhash to Redis
    ///
    /// Note: This is an integration test that requires:
    /// - TEST_POSTGRES_URL environment variable with a test database
    /// - TEST_REDIS_URL environment variable with a test Redis instance
    #[tokio::test]
    #[ignore] // Requires database setup
    async fn test_cache_warming() {
        use std::env;

        // Setup: Get test database URLs from environment
        let postgres_url = env::var("TEST_POSTGRES_URL").unwrap_or_else(|_| {
            "postgresql://contra:contra@localhost:5432/contra_test".to_string()
        });
        let redis_url =
            env::var("TEST_REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string());

        // Create Postgres connection
        let postgres_db = match PostgresAccountsDB::new(&postgres_url, false).await {
            Ok(db) => db,
            Err(e) => {
                eprintln!("Skipping test: Cannot connect to test Postgres: {}", e);
                return;
            }
        };

        // Create Redis connection
        let redis_db = match RedisAccountsDB::new(&redis_url).await {
            Ok(db) => db,
            Err(e) => {
                eprintln!("Skipping test: Cannot connect to test Redis: {}", e);
                return;
            }
        };

        // Setup test data in Postgres
        let test_slot = 12345u64;
        let test_blockhash = Hash::default();
        let test_blockhash_bytes = test_blockhash.to_bytes();

        let pool = postgres_db.pool.clone();

        // Insert test block with slot
        let insert_result = sqlx::query(
            "INSERT INTO blocks (slot, blockhash, previous_blockhash, parent_slot, block_time)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (slot) DO NOTHING",
        )
        .bind(test_slot as i64)
        .bind(test_blockhash_bytes.to_vec())
        .bind(test_blockhash_bytes.to_vec())
        .bind(0i64)
        .bind(0i64)
        .execute(pool.as_ref())
        .await;

        if let Err(e) = insert_result {
            eprintln!(
                "Skipping test: Cannot insert test data into Postgres: {}",
                e
            );
            return;
        }

        // Insert test blockhash into metadata
        let metadata_result = sqlx::query(
            "INSERT INTO metadata (key, value)
             VALUES ('latest_blockhash', $1)
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
        )
        .bind(test_blockhash_bytes.to_vec())
        .execute(pool.as_ref())
        .await;

        if let Err(e) = metadata_result {
            eprintln!("Skipping test: Cannot insert metadata into Postgres: {}", e);
            return;
        }

        // Execute: Call warm_redis_cache
        let result = warm_redis_cache(&postgres_db, &redis_db).await;

        // Verify: Function should succeed
        assert!(
            result.is_ok(),
            "warm_redis_cache should succeed. Got error: {:?}",
            result.err()
        );

        // Verify: Check that Redis was populated correctly
        let mut conn = redis_db.connection.clone();

        // Check latest_slot in Redis
        let redis_slot: Option<u64> = conn.get("latest_slot").await.ok();
        assert_eq!(
            redis_slot,
            Some(test_slot),
            "Redis should contain the correct latest_slot"
        );

        // Check latest_blockhash in Redis
        let redis_blockhash_str: Option<String> = conn.get("latest_blockhash").await.ok();
        assert_eq!(
            redis_blockhash_str,
            Some(test_blockhash.to_string()),
            "Redis should contain the correct latest_blockhash"
        );

        // Cleanup: Remove test data from Postgres
        let _ = sqlx::query("DELETE FROM blocks WHERE slot = $1")
            .bind(test_slot as i64)
            .execute(pool.as_ref())
            .await;

        let _ = sqlx::query("DELETE FROM metadata WHERE key = 'latest_blockhash'")
            .execute(pool.as_ref())
            .await;

        // Cleanup: Remove test data from Redis
        let _: Result<(), _> = conn.del("latest_slot").await;
        let _: Result<(), _> = conn.del("latest_blockhash").await;
    }
}
