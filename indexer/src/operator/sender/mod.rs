mod mint;
mod proof;
mod state;
mod transaction;
pub mod types;

pub use mint::find_existing_mint_signature;
pub use types::TransactionStatusUpdate;

use crate::error::OperatorError;
use crate::operator::utils::instruction_util::TransactionBuilder;
use crate::operator::ReleaseFundsBuilderWithNonce;
use crate::operator::RpcClientWithRetry;
use crate::storage::common::storage::Storage;
use crate::ContraIndexerConfig;
use crate::ProgramType;
use solana_sdk::commitment_config::CommitmentLevel;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};
use tracing::info;

use proof::take_pending_rotation_if_ready;
use transaction::handle_transaction_submission;
use types::SenderState;

/// Sends transactions to the blockchain and updates their status
///
/// Receives TransactionBuilder (either ReleaseFunds or Mint) from processor,
/// completes with SMT proofs if needed, submits to blockchain, and handles failures
#[allow(clippy::too_many_arguments)]
pub async fn run_sender(
    config: &ContraIndexerConfig,
    operator_commitment: CommitmentLevel,
    mut processor_rx: mpsc::Receiver<TransactionBuilder>,
    storage_tx: mpsc::Sender<TransactionStatusUpdate>,
    cancellation_token: tokio_util::sync::CancellationToken,
    storage: Arc<Storage>,
    retry_max_attempts: u32,
    source_rpc_client: Option<Arc<RpcClientWithRetry>>,
) -> Result<(), OperatorError> {
    info!("Starting sender");

    let instance_pda = match config.program_type {
        ProgramType::Withdraw => config.escrow_instance_id,
        ProgramType::Escrow => None,
    };

    let mut state = SenderState::new(
        config,
        operator_commitment,
        instance_pda,
        storage,
        retry_max_attempts,
        source_rpc_client,
    )?;

    // Periodic check for pending rotation (every 500ms)
    let mut rotation_check_interval = interval(Duration::from_millis(500));

    loop {
        tokio::select! {
            _ = cancellation_token.cancelled() => {
                info!("Sender received cancellation signal, draining pipeline...");
                // Continue processing until processor closes the channel
                // This ensures all messages sent by processor before shutdown are handled
                let mut drained_count = 0;
                while let Some(tx_builder) = processor_rx.recv().await {
                    handle_transaction_submission(&mut state, tx_builder, &storage_tx).await;
                    drained_count += 1;
                }
                info!("Sender drained {} remaining transactions", drained_count);
                break;
            }
            _ = rotation_check_interval.tick() => {
                // Check if pending rotation can now be executed
                if let Some(rotation_builder) = take_pending_rotation_if_ready(&mut state) {
                    info!("Executing queued ResetSmtRoot transaction");
                    let tx_builder = TransactionBuilder::ResetSmtRoot(rotation_builder);
                    handle_transaction_submission(&mut state, tx_builder, &storage_tx).await;
                }

                // Process any transactions that were blocked by rotation
                while let Some((ctx, builder)) = state.rotation_retry_queue.pop() {
                    let nonce = ctx.withdrawal_nonce.expect("rotation retry must have nonce");
                    let transaction_id = ctx.transaction_id.expect("rotation retry must have transaction_id");
                    let trace_id = ctx.trace_id.clone().expect("rotation retry must have trace_id");
                    let remint_info = state.remint_cache.get(&nonce).cloned();
                    if remint_info.is_none() {
                        tracing::error!("Missing remint_info for rotation retry nonce {} - remint will not be possible on failure", nonce);
                    }
                    info!(trace_id = %trace_id, "Retrying blocked nonce {} after rotation", nonce);
                    let tx_builder = TransactionBuilder::ReleaseFunds(Box::new(
                        ReleaseFundsBuilderWithNonce { builder, nonce, transaction_id, trace_id, remint_info },
                    ));
                    handle_transaction_submission(&mut state, tx_builder, &storage_tx).await;
                }
            }
            tx_builder = processor_rx.recv() => {
                match tx_builder {
                    Some(tx_builder) => {
                        handle_transaction_submission(&mut state, tx_builder, &storage_tx).await;
                    }
                    None => {
                        info!("Sender channel closed");
                        break;
                    }
                }
            }
        }
    }

    info!("Sender stopped gracefully");
    Ok(())
}
