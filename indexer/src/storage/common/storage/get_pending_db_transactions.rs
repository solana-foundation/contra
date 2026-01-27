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

pub async fn get_pending_db_transactions(
    storage: &Storage,
    transaction_type: TransactionType,
    limit: i64,
) -> Result<Vec<DbTransaction>, StorageError> {
    match storage {
        Storage::Postgres(postgres_db) => {
            get_pending_db_transactions_postgres(postgres_db, transaction_type, limit).await
        }
        #[cfg(test)]
        Storage::Mock(mock_db) => {
            mock_db
                .get_pending_db_transactions(transaction_type, limit)
                .await
        }
    }
}

async fn get_pending_db_transactions_postgres(
    db: &PostgresDb,
    transaction_type: TransactionType,
    limit: i64,
) -> Result<Vec<DbTransaction>, StorageError> {
    Ok(db
        .get_pending_withdrawals_internal(transaction_type, limit)
        .await?)
}
