use crate::storage::common::{
    models::{DbTransaction, TransactionType},
    storage::Storage,
};

pub async fn get_all_db_transactions(
    storage: &Storage,
    transaction_type: TransactionType,
    limit: i64,
) -> Result<Vec<DbTransaction>, Box<dyn std::error::Error + Send + Sync>> {
    match storage {
        Storage::Postgres(db) => Ok(db
            .get_all_transactions_internal(transaction_type, limit)
            .await?),
        #[cfg(any(test, feature = "test-mock-storage"))]
        Storage::Mock(mock) => Ok(mock
            .get_all_db_transactions(transaction_type, limit)
            .await?),
    }
}
