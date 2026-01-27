use crate::{
    error::StorageError,
    storage::{
        common::{models::DbTransaction, storage::Storage},
        postgres::db::PostgresDb,
    },
};

pub async fn insert_db_transactions_batch(
    storage: &Storage,
    transactions: &[DbTransaction],
) -> Result<Vec<i64>, StorageError> {
    match storage {
        Storage::Postgres(postgres_db) => {
            insert_db_transactions_batch_postgres(postgres_db, transactions).await
        }
        #[cfg(test)]
        Storage::Mock(mock_db) => mock_db.insert_db_transactions_batch(transactions).await,
    }
}

async fn insert_db_transactions_batch_postgres(
    db: &PostgresDb,
    transactions: &[DbTransaction],
) -> Result<Vec<i64>, StorageError> {
    Ok(db.insert_transactions_batch_internal(transactions).await?)
}
