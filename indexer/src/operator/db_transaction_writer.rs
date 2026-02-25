use crate::error::StorageError;
use crate::operator::sender::TransactionStatusUpdate;
use crate::storage::common::models::TransactionStatus;
use crate::storage::Storage;
use chrono::Utc;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

/// DbTransactionWriter that receives transaction status updates from sender
/// and writes them to the database
pub struct DbTransactionWriter {
    storage: Arc<Storage>,
    update_rx: mpsc::Receiver<TransactionStatusUpdate>,
    client: reqwest::Client,
    webhook_url: Option<String>,
}

impl DbTransactionWriter {
    pub fn new(
        storage: Arc<Storage>,
        update_rx: mpsc::Receiver<TransactionStatusUpdate>,
        webhook_url: Option<String>,
    ) -> Self {
        let client = reqwest::Client::new();
        Self {
            storage,
            update_rx,
            client,
            webhook_url,
        }
    }

    /// Start processing status updates from the channel
    pub async fn start(mut self) -> Result<(), StorageError> {
        info!("Starting StorageWriter");

        while let Some(update) = self.update_rx.recv().await {
            self.handle_update(update).await;
        }

        info!("StorageWriter stopped");
        Ok(())
    }

    /// Handle a single transaction status update
    async fn handle_update(&self, update: TransactionStatusUpdate) {
        if let Err(e) = self
            .storage
            .update_transaction_status(
                update.transaction_id,
                update.status,
                update.counterpart_signature.clone(),
                update.processed_at.unwrap_or_else(Utc::now),
            )
            .await
        {
            error!(
                "Failed to update transaction {} status: {}",
                update.transaction_id, e
            );
            if let Some(err_msg) = &update.error_message {
                error!("Transaction error was: {}", err_msg);
            }
        } else {
            info!(
                "Updated transaction {} to status {:?}",
                update.transaction_id, update.status
            );

            // Check if transaction failed and send alert
            if update.status == TransactionStatus::Failed {
                // Log failed transaction at ERROR level
                error!(
                    "Transaction {} FAILED",
                    update.transaction_id
                );
                if let Some(err_msg) = &update.error_message {
                    error!("Transaction {} error: {}", update.transaction_id, err_msg);
                }

                // Send webhook alert if configured
                if let Some(webhook_url) = &self.webhook_url {
                    self.send_webhook_alert(webhook_url, &update).await;
                }
            }
        }
    }

    /// Send webhook alert for failed transaction
    async fn send_webhook_alert(&self, webhook_url: &str, update: &TransactionStatusUpdate) {
        let processed_at = update
            .processed_at
            .unwrap_or_else(Utc::now)
            .to_rfc3339();
        let timestamp = Utc::now().to_rfc3339();

        let payload = json!({
            "transaction_id": update.transaction_id,
            "status": "failed",
            "counterpart_signature": update.counterpart_signature,
            "error_message": update.error_message,
            "processed_at": processed_at,
            "timestamp": timestamp,
        });

        match self.client.post(webhook_url).json(&payload).send().await {
            Ok(response) => {
                if response.status().is_success() {
                    info!(
                        "Webhook alert sent successfully for transaction {}",
                        update.transaction_id
                    );
                } else {
                    warn!(
                        "Webhook alert returned non-success status {} for transaction {}: {:?}",
                        response.status(),
                        update.transaction_id,
                        response.text().await
                    );
                }
            }
            Err(e) => {
                warn!(
                    "Failed to send webhook alert for transaction {}: {}",
                    update.transaction_id, e
                );
            }
        }
    }
}
