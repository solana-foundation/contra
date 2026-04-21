use crate::{error::StorageError, storage::common::storage::Storage};

pub async fn reset_transaction_to_pending(
    storage: &Storage,
    transaction_id: i64,
) -> Result<(), StorageError> {
    match storage {
        Storage::Postgres(db) => {
            db.reset_transaction_to_pending_internal(transaction_id)
                .await?;

            Ok(())
        }
        #[cfg(test)]
        Storage::Mock(mock_db) => mock_db.reset_transaction_to_pending(transaction_id).await,
    }
}
