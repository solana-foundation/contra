use crate::{
    error::StorageError,
    storage::{common::storage::Storage, postgres::db::PostgresDb},
};

pub async fn close(storage: &Storage) -> Result<(), StorageError> {
    match storage {
        Storage::Postgres(postgres_db) => close_postgres(postgres_db).await,
        #[cfg(test)]
        Storage::Mock(mock_db) => mock_db.close().await,
    }
}

async fn close_postgres(db: &PostgresDb) -> Result<(), StorageError> {
    db.close().await?;
    Ok(())
}
