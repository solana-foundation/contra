use crate::{
    error::StorageError,
    storage::{
        common::{
            models::DbTransaction,
            storage::Storage,
        },
        postgres::db::PostgresDb,
    },
};

pub async fn get_pending_remint_transactions(
    storage: &Storage,
) -> Result<Vec<DbTransaction>, StorageError> {
    match storage {
        Storage::Postgres(postgres_db) => {
            get_pending_remint_transactions_postgres(postgres_db).await
        }
        #[cfg(test)]
        Storage::Mock(mock_db) => mock_db.get_pending_remint_transactions().await,
    }
}

async fn get_pending_remint_transactions_postgres(
    db: &PostgresDb,
) -> Result<Vec<DbTransaction>, StorageError> {
    Ok(db.get_pending_remint_transactions_internal().await?)
}