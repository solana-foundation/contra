use crate::error::StorageError;
use crate::operator::sender::TransactionStatusUpdate;
use crate::storage::Storage;
use chrono::Utc;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info};

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
                update.counterpart_signature,
                update.processed_at.unwrap_or_else(Utc::now),
            )
            .await
        {
            error!(
                "Failed to update transaction {} status: {}",
                update.transaction_id, e
            );
            if let Some(err_msg) = update.error_message {
                error!("Transaction error was: {}", err_msg);
            }
        } else {
            info!(
                "Updated transaction {} to status {:?}",
                update.transaction_id, update.status
            );
        }
    }
}
