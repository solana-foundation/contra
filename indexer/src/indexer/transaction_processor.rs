use crate::{
    channel_utils::send_guaranteed,
    config::ProgramType,
    error::IndexerError,
    indexer::{
        checkpoint::CheckpointUpdate,
        datasource::common::{
            parser::{EscrowInstruction, WithdrawInstruction},
            types::{InstructionWithMetadata, ProcessorMessage, ProgramInstruction},
        },
    },
    storage::{
        common::models::{DbMint, DbTransaction, DbTransactionBuilder, TransactionType},
        Storage,
    },
};
use std::sync::Arc;
use tokio::sync::mpsc;
use crate::metrics;
use tracing::{debug, error, info};

/// Transaction processor that converts instructions to transactions and saves to DB
/// Tracks slot-level success/failure and emits committed checkpoints
///
/// Current implementation: Sequential slot processing with batch inserts per slot (Option 3)
pub struct TransactionProcessor {
    storage: Arc<Storage>,
    checkpoint_tx: mpsc::Sender<CheckpointUpdate>,
    current_slot: Option<u64>,
    current_program_type: Option<ProgramType>,

    // Buffer all instructions from current slot for batch processing
    current_slot_instructions: Vec<InstructionWithMetadata>,
}

impl TransactionProcessor {
    pub fn new(storage: Arc<Storage>, checkpoint_tx: mpsc::Sender<CheckpointUpdate>) -> Self {
        Self {
            storage,
            checkpoint_tx,
            current_slot: None,
            current_program_type: None,
            current_slot_instructions: Vec::new(),
        }
    }

    /// Start processing messages from the channel
    pub async fn start(
        mut self,
        mut instruction_rx: mpsc::Receiver<ProcessorMessage>,
    ) -> Result<(), IndexerError> {
        info!("Starting TransactionProcessor");

        while let Some(message) = instruction_rx.recv().await {
            match message {
                ProcessorMessage::Instruction(instruction_meta) => {
                    // Buffer instruction for current slot
                    self.current_slot = Some(instruction_meta.slot);
                    self.current_program_type = Some(instruction_meta.program_type);
                    self.current_slot_instructions.push(instruction_meta);
                }
                ProcessorMessage::SlotComplete { slot, program_type } => {
                    // Finalize this slot (save txns + send checkpoint)
                    self.finalize_and_checkpoint(slot, program_type).await;
                }
            }
        }

        info!("TransactionProcessor stopped");
        Ok(())
    }

    /// Finalize and checkpoint a slot
    /// Saves any buffered transactions and always sends checkpoint (even if empty)
    async fn finalize_and_checkpoint(&mut self, slot: u64, program_type: ProgramType) {
        let mut mints = Vec::new();
        let mut transactions = Vec::new();

        for instruction_meta in &self.current_slot_instructions {
            let (mint_opt, transaction_opt) = convert_to_db_models(instruction_meta);

            if let Some(mint) = mint_opt {
                mints.push(mint);
            }

            if let Some(transaction) = transaction_opt {
                transactions.push(transaction);
            }
        }

        let mut send_checkpoint = true;

        // Insert mints FIRST (before transactions that might reference them)
        if !mints.is_empty() {
            info!("Finalizing slot {} with {} mint(s)", slot, mints.len());

            match self.storage.upsert_mints_batch(&mints).await {
                Ok(_) => {
                    info!(
                        "Successfully saved {} mint(s) from slot {}",
                        mints.len(),
                        slot
                    );
                    let pt = format!("{:?}", program_type);
                    metrics::INDEXER_MINTS_SAVED
                        .with_label_values(&[&pt])
                        .inc_by(mints.len() as f64);
                }
                Err(e) => {
                    error!("Failed to save mints from slot {}: {}", slot, e);
                    metrics::INDEXER_SLOT_SAVE_ERRORS
                        .with_label_values(&[&format!("{:?}", program_type)])
                        .inc();
                    send_checkpoint = false;
                }
            }
        }

        if !transactions.is_empty() {
            info!(
                "Finalizing slot {} with {} transactions",
                slot,
                transactions.len()
            );

            match self
                .storage
                .insert_db_transactions_batch(&transactions)
                .await
            {
                Ok(ids) => {
                    info!(
                        "Successfully saved {} transactions from slot {}",
                        ids.len(),
                        slot
                    );
                    let pt = format!("{:?}", program_type);
                    metrics::INDEXER_TRANSACTIONS_SAVED
                        .with_label_values(&[&pt])
                        .inc_by(ids.len() as f64);
                }
                Err(e) => {
                    error!("Failed to save transactions from slot {}: {}", slot, e);
                    metrics::INDEXER_SLOT_SAVE_ERRORS
                        .with_label_values(&[&format!("{:?}", program_type)])
                        .inc();
                    send_checkpoint = false;
                }
            }
        } else {
            // Empty slot, just checkpoint it
            debug!("Finalizing empty slot {}", slot);
        }

        if send_checkpoint {
            const MAX_ATTEMPTS: usize = 3;
            let mut attempt = 0;
            loop {
                let res = send_guaranteed(
                    &self.checkpoint_tx,
                    CheckpointUpdate { program_type, slot },
                    "checkpoint",
                )
                .await;

                match res {
                    Ok(_) => {
                        let pt = format!("{:?}", program_type);
                        metrics::INDEXER_SLOTS_PROCESSED
                            .with_label_values(&[&pt])
                            .inc();
                        metrics::INDEXER_CURRENT_SLOT
                            .with_label_values(&[&pt])
                            .set(slot as f64);
                        break;
                    }
                    Err(e) => {
                        attempt += 1;
                        error!(
                            "Checkpoint send failed for slot {} (attempt {}/{}): {}",
                            slot, attempt, MAX_ATTEMPTS, e
                        );
                        if attempt >= MAX_ATTEMPTS {
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                }
            }
        }

        self.current_slot_instructions.clear();
        self.current_slot = None;
        self.current_program_type = None;
    }
}

/// Convert an instruction to either a DbMint or DbTransaction model
///
/// Returns None for instructions that shouldn't be tracked in the database
fn convert_to_db_models(
    instruction_meta: &InstructionWithMetadata,
) -> (Option<DbMint>, Option<DbTransaction>) {
    let signature = match instruction_meta.signature.as_ref() {
        Some(sig) => sig,
        None => return (None, None),
    };

    match &instruction_meta.instruction {
        ProgramInstruction::Escrow(escrow_ix) => match escrow_ix.as_ref() {
            EscrowInstruction::Deposit { accounts, data } => {
                let recipient = data
                    .recipient
                    .map(|r| r.to_string())
                    .unwrap_or_else(|| accounts.user.to_string());

                (
                    None,
                    Some(
                        DbTransactionBuilder::new(
                            signature.clone(),
                            instruction_meta.slot,
                            accounts.mint.to_string(),
                            data.amount,
                        )
                        .initiator(accounts.user.to_string())
                        .recipient(recipient)
                        .transaction_type(TransactionType::Deposit)
                        .build(),
                    ),
                )
            }
            EscrowInstruction::AllowMint {
                accounts, event, ..
            } => (
                Some(DbMint::new(
                    accounts.mint.to_string(),
                    event.decimals as i16,
                    accounts.token_program.to_string(),
                )),
                None,
            ),
            _ => (None, None),
        },

        ProgramInstruction::Withdraw(withdraw_ix) => match withdraw_ix.as_ref() {
            WithdrawInstruction::WithdrawFunds { accounts, data } => {
                let recipient = data.destination.to_string();

                (
                    None,
                    Some(
                        DbTransactionBuilder::new(
                            signature.clone(),
                            instruction_meta.slot,
                            accounts.mint.to_string(),
                            data.amount,
                        )
                        .initiator(accounts.user.to_string())
                        .recipient(recipient)
                        .transaction_type(TransactionType::Withdrawal)
                        .build(),
                    ),
                )
            }
        },
    }
}
