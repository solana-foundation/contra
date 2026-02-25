use crate::{
    error::StorageError,
    storage::{common::storage::Storage, postgres::db::PostgresDb},
};

pub async fn drop_tables(storage: &Storage) -> Result<(), StorageError> {
    match storage {
        Storage::Postgres(postgres_db) => drop_tables_postgres(postgres_db).await,
        #[cfg(test)]
        Storage::Mock(mock_db) => mock_db.drop_tables().await,
    }
}

async fn drop_tables_postgres(db: &PostgresDb) -> Result<(), StorageError> {
    db.drop_tables().await?;
    Ok(())
}
