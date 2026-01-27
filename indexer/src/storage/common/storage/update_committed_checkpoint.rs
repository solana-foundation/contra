use crate::{
    error::StorageError,
    storage::{common::storage::Storage, postgres::db::PostgresDb},
};

pub async fn update_committed_checkpoint(
    storage: &Storage,
    program_type: &str,
    slot: u64,
) -> Result<(), StorageError> {
    match storage {
        Storage::Postgres(postgres_db) => {
            update_committed_checkpoint_postgres(postgres_db, program_type, slot).await
        }
        #[cfg(test)]
        Storage::Mock(mock_db) => {
            mock_db
                .update_committed_checkpoint(program_type, slot)
                .await
        }
    }
}

async fn update_committed_checkpoint_postgres(
    db: &PostgresDb,
    program_type: &str,
    slot: u64,
) -> Result<(), StorageError> {
    Ok(db
        .update_committed_checkpoint_internal(program_type, slot)
        .await?)
}
