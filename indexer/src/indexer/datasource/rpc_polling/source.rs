use super::rpc::RpcPoller;
use crate::channel_utils::send_guaranteed;
use crate::config::ProgramType;
use crate::error::DataSourceError;
use crate::indexer::datasource::common::{datasource::DataSource, types::*};
use crate::indexer::datasource::rpc_polling::decoder;
use crate::metrics;
use async_trait::async_trait;
use private_channel_metrics::{HealthState, MetricLabel};
use solana_sdk::commitment_config::CommitmentLevel;
use solana_transaction_status::UiTransactionEncoding;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

pub struct RpcPollingSource {
    rpc_url: String,
    from_slot: Option<u64>,
    poll_interval_ms: u64,
    error_retry_interval_ms: u64,
    batch_size: usize,
    encoding: UiTransactionEncoding,
    commitment: CommitmentLevel,
    program_type: ProgramType,
    escrow_instance_id: Option<solana_sdk::pubkey::Pubkey>,
    health: Option<Arc<HealthState>>,
}

impl RpcPollingSource {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        rpc_url: String,
        from_slot: Option<u64>,
        poll_interval_ms: u64,
        error_retry_interval_ms: u64,
        batch_size: usize,
        encoding: UiTransactionEncoding,
        commitment: CommitmentLevel,
        program_type: ProgramType,
        escrow_instance_id: Option<solana_sdk::pubkey::Pubkey>,
    ) -> Self {
        Self {
            rpc_url,
            from_slot,
            poll_interval_ms,
            error_retry_interval_ms,
            batch_size,
            encoding,
            commitment,
            program_type,
            escrow_instance_id,
            health: None,
        }
    }

    pub fn with_health(mut self, health: Arc<HealthState>) -> Self {
        self.health = Some(health);
        self
    }
}

#[async_trait]
impl DataSource for RpcPollingSource {
    async fn start(
        &mut self,
        tx: InstructionSender,
        cancellation_token: CancellationToken,
    ) -> Result<tokio::task::JoinHandle<()>, DataSourceError> {
        let poller = Arc::new(RpcPoller::new(
            self.rpc_url.clone(),
            self.encoding,
            self.commitment,
        ));

        // Current slot is either the from slot or the latest slot
        let mut current_slot = if let Some(slot) = self.from_slot {
            slot
        } else {
            poller
                .get_latest_slot()
                .await
                .map_err(DataSourceError::from)?
        };

        let batch_size = self.batch_size;
        let poll_interval_ms = self.poll_interval_ms;
        let error_retry_interval_ms = self.error_retry_interval_ms;
        let program_type = self.program_type;
        let escrow_instance_id = self.escrow_instance_id;
        let health = self.health.clone();

        let handle = tokio::spawn(async move {
            info!(
                "Starting RPC polling from slot {} for program {:?}",
                current_slot, program_type
            );

            loop {
                // Check for cancellation
                if cancellation_token.is_cancelled() {
                    info!("RPC polling source received cancellation signal, stopping...");
                    break;
                }
                // Get slots to process
                let (slots, chain_tip) =
                    match poller.get_slots_to_process(current_slot, batch_size).await {
                        Ok(result) => result,
                        Err(e) => {
                            {
                                error!("Failed to get slots to process: {}", e);
                                metrics::INDEXER_RPC_ERRORS
                                    .with_label_values(&[program_type.as_label(), "get_slots"])
                                    .inc();
                            }
                            tokio::time::sleep(Duration::from_millis(error_retry_interval_ms))
                                .await;
                            continue;
                        }
                    };
                metrics::INDEXER_CHAIN_TIP_SLOT
                    .with_label_values(&[program_type.as_label()])
                    .set(chain_tip as f64);
                if let Some(h) = &health {
                    h.set_pending(chain_tip.saturating_sub(current_slot));
                }

                // If no slots available, wait and retry
                if slots.is_empty() {
                    tokio::time::sleep(Duration::from_millis(poll_interval_ms)).await;
                    continue;
                }

                // Fetch blocks in batch
                let blocks = poller.get_blocks_batch(slots.clone()).await;

                // Parse and send instructions from each block
                for (slot, block_result) in blocks {
                    match block_result {
                        Ok(Some(block)) => {
                            // Parse program-specific instructions from block with metadata
                            let instructions_with_meta = decoder::parse_block(
                                &block,
                                slot,
                                program_type,
                                escrow_instance_id.as_ref(),
                            );

                            if !instructions_with_meta.is_empty() {
                                info!(
                                    "Slot {}: found {} {:?} instructions",
                                    slot,
                                    instructions_with_meta.len(),
                                    program_type
                                );
                            } else {
                                debug!("Slot {}: found no {:?} instructions", slot, program_type);
                            }

                            for instruction_meta in instructions_with_meta {
                                if let Err(e) = send_guaranteed(
                                    &tx,
                                    ProcessorMessage::Instruction(instruction_meta),
                                    "instruction",
                                )
                                .await
                                {
                                    error!(
                                        "Instruction send failed, stopping RPC polling gracefully: {}",
                                        e
                                    );
                                    break;
                                }
                            }
                        }
                        Ok(None) => {
                            info!("Slot {} was skipped", slot);
                        }
                        Err(e) => {
                            error!("Failed to fetch block {}: {}", slot, e);
                            metrics::INDEXER_RPC_ERRORS
                                .with_label_values(&[program_type.as_label(), "get_block"])
                                .inc();
                        }
                    }

                    // Send SlotComplete marker for this slot
                    let send_res = send_guaranteed(
                        &tx,
                        ProcessorMessage::SlotComplete { slot, program_type },
                        "SlotComplete marker",
                    )
                    .await;
                    if let Err(e) = send_res {
                        error!(
                            "SlotComplete send failed, stopping RPC polling gracefully: {}",
                            e
                        );
                        break;
                    }

                    current_slot = slot + 1;
                }

                // Log progress periodically
                if current_slot.is_multiple_of(1000) {
                    info!(
                        "RPC polling progress: processed up to slot {}",
                        current_slot
                    );
                }
            }

            info!("RPC polling source stopped gracefully");
        });

        Ok(handle)
    }

    async fn shutdown(&mut self) -> Result<(), DataSourceError> {
        info!("RPC polling source shutdown requested (no additional cleanup needed)");
        Ok(())
    }
}
