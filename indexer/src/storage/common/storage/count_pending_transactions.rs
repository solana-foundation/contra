use crate::{
    error::StorageError,
    storage::{
        common::{models::TransactionType, storage::Storage},
        postgres::db::PostgresDb,
    },
};

pub async fn count_pending_transactions(
    storage: &Storage,
    transaction_type: TransactionType,
) -> Result<i64, StorageError> {
    match storage {
        Storage::Postgres(postgres_db) => {
            count_pending_transactions_postgres(postgres_db, transaction_type).await
        }
        #[cfg(test)]
        Storage::Mock(mock_db) => mock_db.count_pending_transactions(transaction_type).await,
    }
}

async fn count_pending_transactions_postgres(
    db: &PostgresDb,
    transaction_type: TransactionType,
) -> Result<i64, StorageError> {
    Ok(db
        .count_pending_transactions_internal(transaction_type)
        .await?)
}
