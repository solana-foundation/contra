use crate::{
    error::StorageError,
    storage::{common::storage::Storage, postgres::db::PostgresDb},
};

pub async fn get_committed_checkpoint(
    storage: &Storage,
    program_type: &str,
) -> Result<Option<u64>, StorageError> {
    match storage {
        Storage::Postgres(postgres_db) => {
            get_committed_checkpoint_postgres(postgres_db, program_type).await
        }
        #[cfg(test)]
        Storage::Mock(mock_db) => mock_db.get_committed_checkpoint(program_type).await,
    }
}

async fn get_committed_checkpoint_postgres(
    db: &PostgresDb,
    program_type: &str,
) -> Result<Option<u64>, StorageError> {
    Ok(db.get_committed_checkpoint_internal(program_type).await?)
}
