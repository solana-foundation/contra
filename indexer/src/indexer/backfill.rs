use crate::metrics;
use crate::{
    channel_utils::send_guaranteed,
    config::{BackfillConfig, ProgramType},
    error::{BackfillError, DataSourceError, IndexerError},
    indexer::{
        checkpoint::get_last_checkpoint,
        datasource::{
            common::types::{InstructionSender, ProcessorMessage},
            rpc_polling::{decoder, rpc::RpcPoller, types::RpcBlock},
        },
    },
    storage::Storage,
};
use contra_metrics::MetricLabel;
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

const BACKFILL_RETRY_DELAY_MS: u64 = 5000;
const BACKFILL_MAX_RETRIES: usize = 3;

/// Validate gap between current slot and a reference slot.
/// Returns Ok(None) if no gap, Ok(Some(gap)) if valid gap, Err if gap too large.
pub fn validate_gap(
    current_slot: u64,
    last_checkpoint: u64,
    max_gap_slots: u64,
) -> Result<Option<u64>, BackfillError> {
    if current_slot <= last_checkpoint {
        return Ok(None);
    }

    let gap = current_slot - last_checkpoint;

    if gap > max_gap_slots {
        return Err(BackfillError::GapTooLarge {
            gap,
            max_gap: max_gap_slots,
        });
    }

    Ok(Some(gap))
}

fn calculate_batches(from_slot: u64, to_slot: u64, batch_size: usize) -> Vec<Vec<u64>> {
    let mut batches = vec![];
    let mut next_slot = from_slot + 1;

    while next_slot <= to_slot {
        let batch_end = std::cmp::min(next_slot + batch_size as u64, to_slot + 1);
        let batch: Vec<u64> = (next_slot..batch_end).collect();
        batches.push(batch);
        next_slot = batch_end;
    }

    batches
}

async fn fetch_blocks_with_retry(
    rpc_poller: &RpcPoller,
    slots: &[u64],
    retry_count: usize,
) -> Result<Vec<(u64, Result<Option<RpcBlock>, BackfillError>)>, IndexerError> {
    if retry_count > 0 {
        tokio::time::sleep(Duration::from_millis(
            BACKFILL_RETRY_DELAY_MS * retry_count as u64,
        ))
        .await;
    }

    Ok(rpc_poller
        .get_blocks_batch(slots.to_vec())
        .await
        .into_iter()
        .map(|(slot, result)| {
            (
                slot,
                result.map_err(|e| BackfillError::SlotFetchFailed { slot, source: e }),
            )
        })
        .collect::<Vec<(u64, Result<Option<RpcBlock>, BackfillError>)>>())
}

/// Fill a range of slots by fetching blocks via RPC and sending parsed instructions.
/// Shared by startup backfill and reconnect gap-fill.
/// Returns the number of processed slots.
pub async fn fill_slot_range(
    rpc_poller: &RpcPoller,
    from_slot: u64,
    to_slot: u64,
    batch_size: usize,
    program_type: ProgramType,
    escrow_instance_id: Option<Pubkey>,
    instruction_tx: &InstructionSender,
) -> Result<u64, IndexerError> {
    let mut processed_count: u64 = 0;
    let gap = to_slot - from_slot;

    metrics::INDEXER_BACKFILL_SLOTS_REMAINING
        .with_label_values(&[program_type.as_label()])
        .set(gap as f64);

    let all_batches = calculate_batches(from_slot, to_slot, batch_size);

    for slots in all_batches {
        let mut retry_count = 0;
        let blocks = loop {
            match fetch_blocks_with_retry(rpc_poller, &slots, retry_count).await {
                Ok(blocks) => break blocks,
                Err(e) => {
                    retry_count += 1;
                    if retry_count >= BACKFILL_MAX_RETRIES {
                        error!(
                            "Failed to fetch blocks after {} retries: {}",
                            BACKFILL_MAX_RETRIES, e
                        );
                        return Err(e);
                    }
                    warn!(
                        "Retry {}/{} after error: {}",
                        retry_count, BACKFILL_MAX_RETRIES, e
                    );
                    tokio::time::sleep(Duration::from_millis(
                        BACKFILL_RETRY_DELAY_MS * retry_count as u64,
                    ))
                    .await;
                }
            }
        };

        for (slot, block_result) in blocks {
            match block_result {
                Ok(Some(block)) => {
                    let instructions_with_meta = decoder::parse_block(
                        &block,
                        slot,
                        program_type,
                        escrow_instance_id.as_ref(),
                    );

                    for instruction_meta in instructions_with_meta {
                        send_guaranteed(
                            instruction_tx,
                            ProcessorMessage::Instruction(instruction_meta),
                            "instruction (backfill)",
                        )
                        .await
                        .map_err(BackfillError::ChannelSend)?;
                    }
                    processed_count += 1;
                }
                Ok(None) => {
                    processed_count += 1;
                }
                Err(e) => {
                    warn!("Error fetching block {}: {}", slot, e);
                    return Err(DataSourceError::from(e).into());
                }
            }

            send_guaranteed(
                instruction_tx,
                ProcessorMessage::SlotComplete { slot, program_type },
                "SlotComplete marker (backfill)",
            )
            .await
            .map_err(|e| DataSourceError::from(BackfillError::ChannelSend(e)))?;
        }

        metrics::INDEXER_BACKFILL_SLOTS_REMAINING
            .with_label_values(&[program_type.as_label()])
            .set((gap - processed_count) as f64);

        if processed_count.is_multiple_of(1000) {
            let progress = ((processed_count as f64 / gap as f64) * 100.0) as u32;
            info!(
                "Backfill progress for {:?}: {}/{} slots ({}%)",
                program_type, processed_count, gap, progress
            );
        }
    }

    metrics::INDEXER_BACKFILL_SLOTS_REMAINING
        .with_label_values(&[program_type.as_label()])
        .set(0.0);

    info!(
        "Backfill complete for {:?}. Processed {} slots from {} to {}",
        program_type, processed_count, from_slot, to_slot
    );
    Ok(processed_count)
}

/// Backfill service for recovering missed slots on startup
pub struct BackfillService {
    storage: Arc<Storage>,
    rpc_poller: Arc<RpcPoller>,
    program_type: ProgramType,
    config: BackfillConfig,
    escrow_instance_id: Option<Pubkey>,
}

impl BackfillService {
    pub fn new(
        storage: Arc<Storage>,
        rpc_poller: Arc<RpcPoller>,
        program_type: ProgramType,
        config: BackfillConfig,
        escrow_instance_id: Option<Pubkey>,
    ) -> Self {
        Self {
            storage,
            rpc_poller,
            program_type,
            config,
            escrow_instance_id,
        }
    }

    /// Run the backfill process
    /// Returns Ok(()) if no gap or backfill successful, Err if gap too large or backfill failed
    pub async fn run(&self, instruction_tx: InstructionSender) -> Result<(), IndexerError> {
        info!(
            "Checking for gaps in indexed data for {:?}...",
            self.program_type
        );

        let last_checkpoint = get_last_checkpoint(&self.storage, self.program_type).await?;

        // Use the larger of configured start_slot and database checkpoint
        // Note: start_slot is inclusive (first slot to process), checkpoint is exclusive (last processed)
        let from_slot = if let Some(configured_start) = self.config.start_slot {
            // Convert inclusive start_slot to exclusive checkpoint format
            let configured_checkpoint = if configured_start > 0 {
                configured_start - 1
            } else {
                0
            };

            let effective_slot = std::cmp::max(configured_checkpoint, last_checkpoint);
            if configured_checkpoint > last_checkpoint {
                info!(
                    "Using configured start_slot {} (will process from slot {}, ahead of database checkpoint {})",
                    configured_start, configured_start, last_checkpoint
                );
            } else {
                info!(
                    "Database checkpoint {} is ahead of configured start_slot {}, using checkpoint",
                    last_checkpoint, configured_start
                );
            }
            effective_slot
        } else {
            last_checkpoint
        };

        let current_slot = self
            .rpc_poller
            .get_latest_slot()
            .await
            .map_err(|e| BackfillError::SlotFetchFailed { slot: 0, source: e })?;

        match validate_gap(current_slot, from_slot, self.config.max_gap_slots)
            .map_err(DataSourceError::from)?
        {
            None => {
                info!(
                    "No gap detected for {:?}. Current slot: {}, From slot: {}",
                    self.program_type, current_slot, from_slot
                );
                return Ok(());
            }
            Some(gap) => {
                info!(
                    "Gap detected for {:?}: {} slots (from {} to {}). Starting backfill...",
                    self.program_type, gap, from_slot, current_slot
                );
            }
        }

        fill_slot_range(
            &self.rpc_poller,
            from_slot,
            current_slot,
            self.config.batch_size,
            self.program_type,
            self.escrow_instance_id,
            &instruction_tx,
        )
        .await?;

        info!("Backfill complete for {:?}", self.program_type);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============================================================================
    // validate_gap Tests
    // ============================================================================

    #[test]
    fn test_validate_gap_no_gap() {
        let result = validate_gap(100, 100, 1000);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None);
    }

    #[test]
    fn test_validate_gap_current_behind_checkpoint() {
        let result = validate_gap(50, 100, 1000);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None);
    }

    #[test]
    fn test_validate_gap_within_limit() {
        let result = validate_gap(150, 100, 1000);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some(50));
    }

    #[test]
    fn test_validate_gap_exceeds_limit() {
        let result = validate_gap(2000, 100, 1000);
        assert!(result.is_err());
        let err_msg = result.unwrap_err();
        let err_str = err_msg.to_string();
        assert!(err_str.contains("Gap too large"), "Error: {}", err_str);
        assert!(err_str.contains("1900 slots"), "Error: {}", err_str);
    }

    #[test]
    fn test_validate_gap_exactly_at_limit() {
        let result = validate_gap(1100, 100, 1000);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some(1000));
    }

    // ============================================================================
    // calculate_batches Tests
    // ============================================================================

    #[test]
    fn test_calculate_batches_full_batches() {
        let batches = calculate_batches(100, 109, 3);

        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0], vec![101, 102, 103]);
        assert_eq!(batches[1], vec![104, 105, 106]);
        assert_eq!(batches[2], vec![107, 108, 109]);
    }

    #[test]
    fn test_calculate_batches_partial_last_batch() {
        let batches = calculate_batches(100, 105, 3);

        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0], vec![101, 102, 103]);
        assert_eq!(batches[1], vec![104, 105]);
    }

    #[test]
    fn test_calculate_batches_single_slot() {
        let batches = calculate_batches(100, 101, 10);

        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0], vec![101]);
    }

    // ============================================================================
    // fill_slot_range Integration Tests
    // ============================================================================

    #[cfg(feature = "datasource-rpc")]
    mod fill_slot_range_tests {
        use super::*;
        use crate::indexer::datasource::rpc_polling::rpc::RpcPoller;
        use mockito::Server;
        use serde_json::json;
        use solana_sdk::commitment_config::CommitmentLevel;
        use solana_transaction_status::UiTransactionEncoding;
        use tokio::sync::mpsc;

        fn empty_block_json() -> serde_json::Value {
            json!({
                "blockhash": "TestBlockHash11111111111111111111111111111",
                "parentSlot": 0,
                "transactions": []
            })
        }

        fn mock_get_block_success(server: &mut Server, slot: u64) -> mockito::Mock {
            server
                .mock("POST", "/")
                .match_body(mockito::Matcher::PartialJson(json!({
                    "method": "getBlock",
                    "params": [slot]
                })))
                .with_status(200)
                .with_body(
                    json!({
                        "jsonrpc": "2.0",
                        "result": empty_block_json(),
                        "id": 1
                    })
                    .to_string(),
                )
                .create()
        }

        fn mock_get_block_skipped(server: &mut Server, slot: u64) -> mockito::Mock {
            server
                .mock("POST", "/")
                .match_body(mockito::Matcher::PartialJson(json!({
                    "method": "getBlock",
                    "params": [slot]
                })))
                .with_status(200)
                .with_body(
                    json!({
                        "jsonrpc": "2.0",
                        "error": { "code": -32009, "message": "Slot was skipped" },
                        "id": 1
                    })
                    .to_string(),
                )
                .create()
        }

        fn mock_get_block_error(server: &mut Server, slot: u64) -> mockito::Mock {
            server
                .mock("POST", "/")
                .match_body(mockito::Matcher::PartialJson(json!({
                    "method": "getBlock",
                    "params": [slot]
                })))
                .with_status(200)
                .with_body(
                    json!({
                        "jsonrpc": "2.0",
                        "error": { "code": -32600, "message": "Invalid request" },
                        "id": 1
                    })
                    .to_string(),
                )
                .create()
        }

        #[tokio::test]
        async fn fill_slot_range_empty_blocks() {
            let mut server = Server::new_async().await;

            let _m1 = mock_get_block_success(&mut server, 101);
            let _m2 = mock_get_block_success(&mut server, 102);
            let _m3 = mock_get_block_success(&mut server, 103);

            let poller = RpcPoller::new(
                server.url(),
                UiTransactionEncoding::Json,
                CommitmentLevel::Finalized,
            );

            let (tx, mut rx) = mpsc::channel(64);
            let result =
                fill_slot_range(&poller, 100, 103, 10, ProgramType::Escrow, None, &tx).await;

            assert_eq!(result.unwrap(), 3);
            drop(tx);

            let mut messages = vec![];
            while let Some(msg) = rx.recv().await {
                messages.push(msg);
            }

            assert_eq!(messages.len(), 3);
            for (i, msg) in messages.iter().enumerate() {
                match msg {
                    ProcessorMessage::SlotComplete { slot, .. } => {
                        assert_eq!(*slot, 101 + i as u64);
                    }
                    ProcessorMessage::Instruction(_) => {
                        panic!("Expected no Instruction messages for empty blocks");
                    }
                }
            }
        }

        #[tokio::test]
        async fn fill_slot_range_skipped_slots() {
            let mut server = Server::new_async().await;

            let _m1 = mock_get_block_skipped(&mut server, 101);
            let _m2 = mock_get_block_skipped(&mut server, 102);

            let poller = RpcPoller::new(
                server.url(),
                UiTransactionEncoding::Json,
                CommitmentLevel::Finalized,
            );

            let (tx, mut rx) = mpsc::channel(64);
            let result =
                fill_slot_range(&poller, 100, 102, 10, ProgramType::Escrow, None, &tx).await;

            assert_eq!(result.unwrap(), 2);
            drop(tx);

            let mut messages = vec![];
            while let Some(msg) = rx.recv().await {
                messages.push(msg);
            }

            assert_eq!(messages.len(), 2);
            for msg in &messages {
                assert!(matches!(msg, ProcessorMessage::SlotComplete { .. }));
            }
        }

        #[tokio::test]
        async fn fill_slot_range_block_fetch_error() {
            let mut server = Server::new_async().await;

            let _m1 = mock_get_block_error(&mut server, 101);

            let poller = RpcPoller::new(
                server.url(),
                UiTransactionEncoding::Json,
                CommitmentLevel::Finalized,
            );

            let (tx, _rx) = mpsc::channel(64);
            let result =
                fill_slot_range(&poller, 100, 101, 10, ProgramType::Escrow, None, &tx).await;

            assert!(result.is_err());
        }

        #[tokio::test]
        async fn fill_slot_range_no_slots_in_range() {
            let server = Server::new_async().await;

            let poller = RpcPoller::new(
                server.url(),
                UiTransactionEncoding::Json,
                CommitmentLevel::Finalized,
            );

            let (tx, _rx) = mpsc::channel(64);
            let result =
                fill_slot_range(&poller, 100, 100, 10, ProgramType::Escrow, None, &tx).await;

            assert_eq!(result.unwrap(), 0);
        }
    }
}
