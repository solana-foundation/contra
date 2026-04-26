use crate::{
    error::StorageError,
    storage::common::{models::DbTransaction, storage::Storage},
};

pub async fn insert_db_transaction(
    storage: &Storage,
    transaction: &DbTransaction,
) -> Result<i64, StorageError> {
    match storage {
        Storage::Postgres(db) => Ok(db.insert_transaction_internal(transaction).await?),
        #[cfg(any(test, feature = "test-mock-storage"))]
        Storage::Mock(mock_db) => mock_db.insert_db_transaction(transaction).await,
    }
}
