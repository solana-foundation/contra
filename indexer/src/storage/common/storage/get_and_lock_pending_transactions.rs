use crate::{
    error::StorageError,
    storage::{
        common::{
            models::{DbTransaction, TransactionType},
            storage::Storage,
        },
        postgres::db::PostgresDb,
    },
};

pub async fn get_and_lock_pending_transactions(
    storage: &Storage,
    transaction_type: TransactionType,
    limit: i64,
) -> Result<Vec<DbTransaction>, StorageError> {
    match storage {
        Storage::Postgres(postgres_db) => {
            get_and_lock_pending_transactions_postgres(postgres_db, transaction_type, limit).await
        }
        #[cfg(test)]
        Storage::Mock(mock_db) => {
            mock_db
                .get_and_lock_pending_transactions(transaction_type, limit)
                .await
        }
    }
}

async fn get_and_lock_pending_transactions_postgres(
    db: &PostgresDb,
    transaction_type: TransactionType,
    limit: i64,
) -> Result<Vec<DbTransaction>, StorageError> {
    Ok(db
        .get_and_lock_pending_transactions_internal(transaction_type, limit)
        .await?)
}
