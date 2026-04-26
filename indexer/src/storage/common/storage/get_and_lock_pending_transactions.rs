use crate::{
    error::StorageError,
    storage::common::{
        models::{DbTransaction, TransactionType},
        storage::Storage,
    },
};

pub async fn get_and_lock_pending_transactions(
    storage: &Storage,
    transaction_type: TransactionType,
    limit: i64,
) -> Result<Vec<DbTransaction>, StorageError> {
    match storage {
        Storage::Postgres(db) => Ok(db
            .get_and_lock_pending_transactions_internal(transaction_type, limit)
            .await?),
        #[cfg(any(test, feature = "test-mock-storage"))]
        Storage::Mock(mock_db) => {
            mock_db
                .get_and_lock_pending_transactions(transaction_type, limit)
                .await
        }
    }
}
