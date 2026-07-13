use crate::{error::StorageError, storage::common::storage::Storage};

/// Read a row's durable requeue counter; `None` if the row does not exist.
pub async fn get_recovery_requeue_attempts(
    storage: &Storage,
    transaction_id: i64,
) -> Result<Option<i32>, StorageError> {
    match storage {
        Storage::Postgres(db) => Ok(db
            .get_recovery_requeue_attempts_internal(transaction_id)
            .await?),
        #[cfg(any(test, feature = "test-mock-storage"))]
        Storage::Mock(mock_db) => mock_db.get_recovery_requeue_attempts(transaction_id).await,
    }
}
