use crate::{error::StorageError, storage::common::storage::Storage};

pub async fn set_pending_remint(
    storage: &Storage,
    transaction_id: i64,
    remint_signatures: Vec<String>,
    deadline_at: chrono::DateTime<chrono::Utc>,
) -> Result<(), StorageError> {
    match storage {
        Storage::Postgres(db) => {
            db.set_pending_remint_internal(transaction_id, remint_signatures, deadline_at)
                .await?;

            Ok(())
        }
        #[cfg(any(test, feature = "test-mock-storage"))]
        Storage::Mock(mock_db) => {
            mock_db
                .set_pending_remint(transaction_id, remint_signatures, deadline_at)
                .await
        }
    }
}
