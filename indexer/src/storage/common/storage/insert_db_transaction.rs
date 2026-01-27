use crate::{
    error::StorageError,
    storage::{
        common::{models::DbTransaction, storage::Storage},
        postgres::db::PostgresDb,
    },
};

pub async fn insert_db_transaction(
    storage: &Storage,
    transaction: &DbTransaction,
) -> Result<i64, StorageError> {
    match storage {
        Storage::Postgres(postgres_db) => {
            insert_db_transaction_postgres(postgres_db, transaction).await
        }
        #[cfg(test)]
        Storage::Mock(mock_db) => mock_db.insert_db_transaction(transaction).await,
    }
}

async fn insert_db_transaction_postgres(
    db: &PostgresDb,
    transaction: &DbTransaction,
) -> Result<i64, StorageError> {
    Ok(db.insert_transaction_internal(transaction).await?)
}
