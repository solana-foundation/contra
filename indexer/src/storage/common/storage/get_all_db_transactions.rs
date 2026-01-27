use crate::storage::{
    common::{
        models::{DbTransaction, TransactionType},
        storage::Storage,
    },
    postgres::db::PostgresDb,
};

/// Get all transactions of a given type regardless of status
/// Useful for querying transactions that may have been processed by the operator
pub async fn get_all_db_transactions(
    storage: &Storage,
    transaction_type: TransactionType,
    limit: i64,
) -> Result<Vec<DbTransaction>, Box<dyn std::error::Error + Send + Sync>> {
    match storage {
        Storage::Postgres(postgres_db) => {
            get_all_db_transactions_postgres(postgres_db, transaction_type, limit).await
        }
        #[cfg(test)]
        Storage::Mock(_mock_db) => {
            // Mock storage doesn't track transactions, return empty vec
            Ok(vec![])
        }
    }
}

async fn get_all_db_transactions_postgres(
    db: &PostgresDb,
    transaction_type: TransactionType,
    limit: i64,
) -> Result<Vec<DbTransaction>, Box<dyn std::error::Error + Send + Sync>> {
    Ok(db
        .get_all_transactions_internal(transaction_type, limit)
        .await?)
}
