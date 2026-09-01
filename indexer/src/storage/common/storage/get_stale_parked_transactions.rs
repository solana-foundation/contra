use crate::{
    error::StorageError,
    storage::common::{
        models::{DbTransaction, TransactionType},
        storage::Storage,
    },
};

/// Stale `Parked` rows of one type older than the threshold, oldest-first.
pub async fn get_stale_parked_transactions(
    storage: &Storage,
    transaction_type: TransactionType,
    threshold: std::time::Duration,
    limit: i64,
) -> Result<Vec<DbTransaction>, StorageError> {
    match storage {
        Storage::Postgres(db) => Ok(db
            .get_stale_parked_transactions_internal(transaction_type, threshold, limit)
            .await?),
        #[cfg(any(test, feature = "test-mock-storage"))]
        Storage::Mock(mock_db) => {
            mock_db
                .get_stale_parked_transactions(transaction_type, threshold, limit)
                .await
        }
    }
}
