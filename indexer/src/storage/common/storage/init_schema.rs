use crate::{
    error::StorageError,
    storage::{common::storage::Storage, postgres::db::PostgresDb},
};

pub async fn init_schema(storage: &Storage) -> Result<(), StorageError> {
    match storage {
        Storage::Postgres(postgres_db) => init_schema_postgres(postgres_db).await,
        #[cfg(test)]
        Storage::Mock(mock_db) => mock_db.init_schema().await,
    }
}

async fn init_schema_postgres(db: &PostgresDb) -> Result<(), StorageError> {
    db.init_schema().await?;
    Ok(())
}
