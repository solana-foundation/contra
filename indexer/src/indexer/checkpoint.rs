use crate::{config::ProgramType, error::CheckpointError, storage::Storage};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::interval;
use tracing::{error, info, warn};

/// Checkpoint update message sent by transaction processor
/// Indicates that a slot has been fully processed (transactions saved or confirmed empty)
#[derive(Debug, Clone)]
pub struct CheckpointUpdate {
    pub program_type: ProgramType,
    pub slot: u64,
}

/// Checkpoint writer service that batches and persists checkpoint updates
pub struct CheckpointWriter {
    storage: Arc<Storage>,
    batch_interval_secs: u64,
    max_batch_size: usize,
}

/// Pending checkpoints waiting to be flushed to storage
type PendingCheckpoints = HashMap<ProgramType, u64>;

impl CheckpointWriter {
    pub fn new(storage: Arc<Storage>) -> Self {
        Self {
            storage,
            batch_interval_secs: 5, // Write every 5 seconds
            max_batch_size: 100,    // Or every 100 updates
        }
    }

    pub fn with_batch_interval(mut self, seconds: u64) -> Self {
        self.batch_interval_secs = seconds;
        self
    }

    pub fn with_max_batch_size(mut self, size: usize) -> Self {
        self.max_batch_size = size;
        self
    }

    /// Start the checkpoint writer service
    /// Spawns a background task that listens for checkpoint updates and batches writes to DB
    pub fn start(self, mut rx: mpsc::Receiver<CheckpointUpdate>) -> JoinHandle<()> {
        tokio::spawn(async move {
            info!(
                "Starting CheckpointWriter service (batch interval: {}s, max batch size: {})",
                self.batch_interval_secs, self.max_batch_size
            );

            let mut pending: PendingCheckpoints = HashMap::new();
            let mut update_count = 0;

            let mut ticker = interval(Duration::from_secs(self.batch_interval_secs));
            ticker.tick().await; // First tick completes immediately

            loop {
                tokio::select! {
                    Some(update) = rx.recv() => {
                        pending
                            .entry(update.program_type)
                            .and_modify(|slot| {
                                if update.slot > *slot {
                                    *slot = update.slot;
                                }
                            })
                            .or_insert(update.slot);

                        update_count += 1;

                        if update_count >= self.max_batch_size {
                            if let Err(e) = self.flush_checkpoints(&mut pending).await {
                                error!("Failed to flush checkpoints: {}", e);
                            }
                            update_count = 0;
                        }
                    }

                    _ = ticker.tick() => {
                        if !pending.is_empty() {
                            if let Err(e) = self.flush_checkpoints(&mut pending).await {
                                error!("Failed to flush checkpoints on timer: {}", e);
                            }
                            update_count = 0;
                        }
                    }

                    else => {
                        info!("Checkpoint channel closed, flushing remaining checkpoints");
                        if !pending.is_empty() {
                            if let Err(e) = self.flush_checkpoints(&mut pending).await {
                                error!("Failed to flush checkpoints on shutdown: {}", e);
                            }
                        }
                        break;
                    }
                }
            }

            info!("CheckpointWriter service stopped");
        })
    }

    /// Flush all pending checkpoints to storage
    /// Only removes checkpoints from pending after successful DB write
    async fn flush_checkpoints(
        &self,
        pending: &mut PendingCheckpoints,
    ) -> Result<(), CheckpointError> {
        // Track which checkpoints were successfully written
        let mut success = Vec::new();

        // Flush all checkpoints
        for (&program_type, &slot) in pending.iter() {
            let program_type_str = format!("{:?}", program_type).to_lowercase();

            match self
                .storage
                .update_committed_checkpoint(&program_type_str, slot)
                .await
            {
                Ok(_) => {
                    info!("Checkpoint updated: {:?} -> slot {}", program_type, slot);
                    success.push(program_type);
                }
                Err(e) => {
                    warn!(
                        "Failed to update checkpoint for {:?} at slot {}: {}",
                        program_type, slot, e
                    );
                }
            }
        }

        // Only remove successfully written checkpoints
        for program_type in success {
            pending.remove(&program_type);
        }

        Ok(())
    }
}

/// Helper to get the last checkpoint for a program type
pub async fn get_last_checkpoint(
    storage: &Arc<Storage>,
    program_type: ProgramType,
) -> Result<u64, CheckpointError> {
    let program_type_str = format!("{:?}", program_type).to_lowercase();
    let checkpoint = storage
        .get_committed_checkpoint(&program_type_str)
        .await?
        .unwrap_or(0);

    info!("Last checkpoint for {:?}: {}", program_type, checkpoint);
    Ok(checkpoint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::common::storage::mock::MockStorage;

    // ============================================================================
    // Builder Tests
    // ============================================================================

    #[test]
    fn test_builder_with_batch_interval() {
        let storage: Arc<Storage> = Arc::new(Storage::Mock(MockStorage::new()));
        let writer = CheckpointWriter::new(storage).with_batch_interval(10);

        assert_eq!(writer.batch_interval_secs, 10);
    }

    #[test]
    fn test_builder_with_max_batch_size() {
        let storage: Arc<Storage> = Arc::new(Storage::Mock(MockStorage::new()));
        let writer = CheckpointWriter::new(storage).with_max_batch_size(50);

        assert_eq!(writer.max_batch_size, 50);
    }

    #[test]
    fn test_builder_chaining() {
        let storage: Arc<Storage> = Arc::new(Storage::Mock(MockStorage::new()));
        let writer = CheckpointWriter::new(storage)
            .with_batch_interval(15)
            .with_max_batch_size(75);

        assert_eq!(writer.batch_interval_secs, 15);
        assert_eq!(writer.max_batch_size, 75);
    }

    // ============================================================================
    // flush_checkpoints Tests
    // ============================================================================

    #[tokio::test]
    async fn test_flush_checkpoints_success() {
        let mock = MockStorage::new();
        let storage = Arc::new(Storage::Mock(mock.clone()));
        let writer = CheckpointWriter::new(storage.clone());

        let mut pending = HashMap::new();
        pending.insert(ProgramType::Escrow, 100);
        pending.insert(ProgramType::Withdraw, 200);

        let result = writer.flush_checkpoints(&mut pending).await;

        assert!(result.is_ok());
        assert!(pending.is_empty());

        // Verify checkpoints were written
        let escrow_checkpoint = storage
            .get_committed_checkpoint("escrow")
            .await
            .unwrap()
            .unwrap();
        let withdraw_checkpoint = storage
            .get_committed_checkpoint("withdraw")
            .await
            .unwrap()
            .unwrap();

        assert_eq!(escrow_checkpoint, 100);
        assert_eq!(withdraw_checkpoint, 200);
    }

    #[tokio::test]
    async fn test_flush_checkpoints_partial_failure() {
        let mock = MockStorage::new();
        mock.set_should_fail("escrow", true); // Escrow will fail
        let storage = Arc::new(Storage::Mock(mock.clone()));
        let writer = CheckpointWriter::new(storage.clone());

        let mut pending = HashMap::new();
        pending.insert(ProgramType::Escrow, 100);
        pending.insert(ProgramType::Withdraw, 200);

        let result = writer.flush_checkpoints(&mut pending).await;

        assert!(result.is_ok()); // flush_checkpoints itself succeeds

        // Failed checkpoint should remain in pending
        assert_eq!(pending.len(), 1);
        assert_eq!(pending.get(&ProgramType::Escrow), Some(&100));

        // Successful checkpoint should be written
        let withdraw_checkpoint = storage
            .get_committed_checkpoint("withdraw")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(withdraw_checkpoint, 200);

        // Failed checkpoint should not be written
        let escrow_checkpoint = storage.get_committed_checkpoint("escrow").await.unwrap();
        assert_eq!(escrow_checkpoint, None);
    }

    #[tokio::test]
    async fn test_flush_checkpoints_empty_pending() {
        let storage = Arc::new(Storage::Mock(MockStorage::new()));
        let writer = CheckpointWriter::new(storage);

        let mut pending = HashMap::new();

        let result = writer.flush_checkpoints(&mut pending).await;

        assert!(result.is_ok());
        assert!(pending.is_empty());
    }

    // ============================================================================
    // get_last_checkpoint Tests
    // ============================================================================

    #[tokio::test]
    async fn test_get_last_checkpoint_exists() {
        let mock = MockStorage::new();
        mock.set_checkpoint("escrow", 12345);
        let storage: Arc<Storage> = Arc::new(Storage::Mock(mock));

        let checkpoint = get_last_checkpoint(&storage, ProgramType::Escrow)
            .await
            .unwrap();

        assert_eq!(checkpoint, 12345);
    }

    #[tokio::test]
    async fn test_get_last_checkpoint_defaults_to_zero() {
        let storage: Arc<Storage> = Arc::new(Storage::Mock(MockStorage::new()));

        let checkpoint = get_last_checkpoint(&storage, ProgramType::Escrow)
            .await
            .unwrap();

        assert_eq!(checkpoint, 0);
    }

    #[tokio::test]
    async fn test_get_last_checkpoint_multiple_program_types() {
        let mock = MockStorage::new();
        mock.set_checkpoint("escrow", 100);
        mock.set_checkpoint("withdraw", 200);
        let storage: Arc<Storage> = Arc::new(Storage::Mock(mock));

        let escrow_checkpoint = get_last_checkpoint(&storage, ProgramType::Escrow)
            .await
            .unwrap();
        let withdraw_checkpoint = get_last_checkpoint(&storage, ProgramType::Withdraw)
            .await
            .unwrap();

        assert_eq!(escrow_checkpoint, 100);
        assert_eq!(withdraw_checkpoint, 200);
    }
}
