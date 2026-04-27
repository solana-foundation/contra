use crate::{
    error::StorageError,
    storage::common::{models::TransactionType, storage::Storage},
};

pub async fn count_pending_transactions(
    storage: &Storage,
    transaction_type: TransactionType,
) -> Result<i64, StorageError> {
    match storage {
        Storage::Postgres(db) => Ok(db
            .count_pending_transactions_internal(transaction_type)
            .await?),
        #[cfg(any(test, feature = "test-mock-storage"))]
        Storage::Mock(mock_db) => mock_db.count_pending_transactions(transaction_type).await,
    }
}
