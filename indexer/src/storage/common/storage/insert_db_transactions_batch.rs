use crate::{
    error::StorageError,
    storage::common::{models::DbTransaction, storage::Storage},
};

pub async fn insert_db_transactions_batch(
    storage: &Storage,
    transactions: &[DbTransaction],
) -> Result<Vec<i64>, StorageError> {
    match storage {
        Storage::Postgres(db) => Ok(db.insert_transactions_batch_internal(transactions).await?),
        #[cfg(any(test, feature = "test-mock-storage"))]
        Storage::Mock(mock_db) => mock_db.insert_db_transactions_batch(transactions).await,
    }
}
