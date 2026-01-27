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
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

const BACKFILL_RETRY_DELAY_MS: u64 = 5000;
const BACKFILL_MAX_RETRIES: usize = 3;

/// Backfill service for recovering missed slots on startup
pub struct BackfillService {
    storage: Arc<Storage>,
    rpc_poller: Arc<RpcPoller>,
    program_type: ProgramType,
    config: BackfillConfig,
    escrow_instance_id: Option<solana_sdk::pubkey::Pubkey>,
}

impl BackfillService {
    pub fn new(
        storage: Arc<Storage>,
        rpc_poller: Arc<RpcPoller>,
        program_type: ProgramType,
        config: BackfillConfig,
        escrow_instance_id: Option<solana_sdk::pubkey::Pubkey>,
    ) -> Self {
        Self {
            storage,
            rpc_poller,
            program_type,
            config,
            escrow_instance_id,
        }
    }

    /// Validate gap between current slot and checkpoint
    /// Returns Ok(None) if no gap, Ok(Some(gap)) if valid gap, Err if gap too large
    fn validate_gap(
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

    /// Calculate slot batches for backfill processing
    /// Returns vector of slot ranges to process in batches
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

        let current_slot = self.rpc_poller.get_latest_slot().await.map_err(|e| {
            BackfillError::SlotFetchFailed {
                slot: 0, // Latest slot fetch failed
                source: e,
            }
        })?;

        match Self::validate_gap(current_slot, from_slot, self.config.max_gap_slots)
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

        self.backfill_gap(from_slot, current_slot, instruction_tx)
            .await?;

        info!("Backfill complete for {:?}", self.program_type);
        Ok(())
    }

    /// Backfill gap using RPC polling
    async fn backfill_gap(
        &self,
        from_slot: u64,
        to_slot: u64,
        instruction_tx: InstructionSender,
    ) -> Result<(), IndexerError> {
        let mut processed_count = 0;
        let gap = to_slot - from_slot;

        let all_batches = Self::calculate_batches(from_slot, to_slot, self.config.batch_size);

        for slots in all_batches {
            let mut retry_count = 0;
            let blocks = loop {
                match self.fetch_blocks_with_retry(&slots, retry_count).await {
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
                            self.program_type,
                            self.escrow_instance_id.as_ref(),
                        );

                        for instruction_meta in instructions_with_meta {
                            send_guaranteed(
                                &instruction_tx,
                                ProcessorMessage::Instruction(instruction_meta),
                                "instruction (backfill)",
                            )
                            .await
                            .map_err(BackfillError::ChannelSend)?;
                        }
                        processed_count += 1;
                    }
                    Ok(None) => {
                        // Skipped slot (valid)
                        processed_count += 1;
                    }
                    Err(e) => {
                        warn!("Error fetching block {}: {}", slot, e);
                        return Err(DataSourceError::from(e).into());
                    }
                }

                // Send SlotComplete marker after processing each slot
                send_guaranteed(
                    &instruction_tx,
                    ProcessorMessage::SlotComplete {
                        slot,
                        program_type: self.program_type,
                    },
                    "SlotComplete marker (backfill)",
                )
                .await
                .map_err(|e| DataSourceError::from(BackfillError::ChannelSend(e)))?;
            }

            if processed_count % 1000 == 0 {
                let progress = ((processed_count as f64 / gap as f64) * 100.0) as u32;
                info!(
                    "Backfill progress for {:?}: {}/{} slots ({}%)",
                    self.program_type, processed_count, gap, progress
                );
            }
        }

        info!(
            "Backfill complete for {:?}. Processed {} slots from {} to {}",
            self.program_type, processed_count, from_slot, to_slot
        );
        Ok(())
    }

    /// Fetch blocks with retry logic
    async fn fetch_blocks_with_retry(
        &self,
        slots: &[u64],
        retry_count: usize,
    ) -> Result<Vec<(u64, Result<Option<RpcBlock>, BackfillError>)>, IndexerError> {
        if retry_count > 0 {
            tokio::time::sleep(Duration::from_millis(
                BACKFILL_RETRY_DELAY_MS * retry_count as u64,
            ))
            .await;
        }

        Ok(self
            .rpc_poller
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
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============================================================================
    // validate_gap Tests
    // ============================================================================

    #[test]
    fn test_validate_gap_no_gap() {
        let result = BackfillService::validate_gap(100, 100, 1000);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None);
    }

    #[test]
    fn test_validate_gap_current_behind_checkpoint() {
        let result = BackfillService::validate_gap(50, 100, 1000);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None);
    }

    #[test]
    fn test_validate_gap_within_limit() {
        let result = BackfillService::validate_gap(150, 100, 1000);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some(50));
    }

    #[test]
    fn test_validate_gap_exceeds_limit() {
        let result = BackfillService::validate_gap(2000, 100, 1000);
        assert!(result.is_err());
        let err_msg = result.unwrap_err();
        let err_str = err_msg.to_string();
        assert!(err_str.contains("Gap too large"), "Error: {}", err_str);
        assert!(err_str.contains("1900 slots"), "Error: {}", err_str);
    }

    #[test]
    fn test_validate_gap_exactly_at_limit() {
        let result = BackfillService::validate_gap(1100, 100, 1000);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some(1000));
    }

    // ============================================================================
    // calculate_batches Tests
    // ============================================================================

    #[test]
    fn test_calculate_batches_full_batches() {
        let batches = BackfillService::calculate_batches(100, 109, 3);

        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0], vec![101, 102, 103]);
        assert_eq!(batches[1], vec![104, 105, 106]);
        assert_eq!(batches[2], vec![107, 108, 109]);
    }

    #[test]
    fn test_calculate_batches_partial_last_batch() {
        let batches = BackfillService::calculate_batches(100, 105, 3);

        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0], vec![101, 102, 103]);
        assert_eq!(batches[1], vec![104, 105]);
    }

    #[test]
    fn test_calculate_batches_single_slot() {
        let batches = BackfillService::calculate_batches(100, 101, 10);

        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0], vec![101]);
    }
}
