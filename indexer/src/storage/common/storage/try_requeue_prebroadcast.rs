use crate::{error::StorageError, storage::common::storage::Storage};

/// Status-only CAS `Processing` → `Pending`; `Ok(false)` if the row is not `Processing`.
pub async fn try_requeue_prebroadcast(
    storage: &Storage,
    transaction_id: i64,
) -> Result<bool, StorageError> {
    match storage {
        Storage::Postgres(db) => Ok(db.try_requeue_prebroadcast_internal(transaction_id).await?),
        #[cfg(any(test, feature = "test-mock-storage"))]
        Storage::Mock(mock_db) => mock_db.try_requeue_prebroadcast(transaction_id).await,
    }
}
