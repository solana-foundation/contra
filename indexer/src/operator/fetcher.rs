use crate::channel_utils::send_guaranteed;
use crate::config::OperatorConfig;
use crate::error::OperatorError;
use crate::metrics;
use crate::storage::common::models::{DbTransaction, TransactionType};
use crate::storage::Storage;
use crate::ProgramType;
use contra_metrics::MetricLabel;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

/// Fetches pending transactions from the database and sends them to the processor
///
/// Uses row-level locking (FOR UPDATE SKIP LOCKED) to ensure only one operator
/// processes a transaction at a time in distributed setups
pub async fn run_fetcher(
    storage: Arc<Storage>,
    processor_tx: mpsc::Sender<DbTransaction>,
    config: OperatorConfig,
    program_type: ProgramType,
    cancellation_token: CancellationToken,
) -> Result<(), OperatorError> {
    info!("Starting fetcher");

    let transaction_type = match program_type {
        ProgramType::Escrow => TransactionType::Deposit,
        ProgramType::Withdraw => TransactionType::Withdrawal,
    };

    loop {
        // Check for cancellation
        if cancellation_token.is_cancelled() {
            info!("Fetcher received cancellation signal, stopping...");
            break;
        }
        if let Ok(count) = storage.count_pending_transactions(transaction_type).await {
            metrics::OPERATOR_BACKLOG_DEPTH
                .with_label_values(&[program_type.as_label()])
                .set(count as f64);
        }

        match storage
            .get_and_lock_pending_transactions(transaction_type, config.batch_size as i64)
            .await
        {
            Ok(transactions) => {
                if !transactions.is_empty() {
                    info!("Fetched {} pending transactions", transactions.len());
                    metrics::OPERATOR_TRANSACTIONS_FETCHED
                        .with_label_values(&[program_type.as_label()])
                        .inc_by(transactions.len() as f64);

                    for transaction in transactions {
                        info!(
                            trace_id = %transaction.trace_id,
                            signature = %transaction.signature,
                            "Sending transaction to processor"
                        );
                        if let Err(e) = send_guaranteed(
                            &processor_tx,
                            transaction.clone(),
                            &format!("transaction {}", transaction.signature),
                        )
                        .await
                        {
                            error!(
                                "Failed to send transaction {} to processor: {}",
                                transaction.signature, e
                            );
                            return Err(OperatorError::ChannelClosed {
                                component: "fetcher".to_string(),
                            });
                        }
                    }
                }
            }
            Err(e) => {
                warn!("Failed to fetch pending transactions: {}", e);
            }
        }

        // Sleep between polls
        tokio::time::sleep(config.db_poll_interval).await;
    }

    info!("Fetcher stopped gracefully");
    Ok(())
}
