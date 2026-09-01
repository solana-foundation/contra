use crate::{
    error::StorageError,
    storage::common::{
        models::{DbTransaction, TransactionType},
        storage::Storage,
    },
};
use std::time::Duration;

/// Stale `Processing` rows of one type past the threshold, oldest-first.
pub async fn get_stale_processing_transactions(
    storage: &Storage,
    transaction_type: TransactionType,
    threshold: Duration,
    limit: i64,
) -> Result<Vec<DbTransaction>, StorageError> {
    match storage {
        Storage::Postgres(db) => Ok(db
            .get_stale_processing_transactions_internal(transaction_type, threshold, limit)
            .await?),
        #[cfg(any(test, feature = "test-mock-storage"))]
        Storage::Mock(mock_db) => {
            mock_db
                .get_stale_processing_transactions(transaction_type, threshold, limit)
                .await
        }
    }
}
