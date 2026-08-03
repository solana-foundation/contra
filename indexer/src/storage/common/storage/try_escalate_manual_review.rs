use crate::{error::StorageError, storage::common::storage::Storage};

/// Escalate a still-`Processing` row to ManualReview. `Ok(false)` means the row
/// is no longer Processing, so this escalation does not own it.
pub async fn try_escalate_manual_review(
    storage: &Storage,
    transaction_id: i64,
) -> Result<bool, StorageError> {
    match storage {
        Storage::Postgres(db) => {
            let escalated = db
                .try_escalate_manual_review_internal(transaction_id)
                .await?;

            Ok(escalated)
        }
        #[cfg(any(test, feature = "test-mock-storage"))]
        Storage::Mock(mock_db) => mock_db.try_escalate_manual_review(transaction_id).await,
    }
}
