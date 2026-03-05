use crate::{error::StorageError, storage::common::storage::Storage};

pub async fn get_committed_checkpoint(
    storage: &Storage,
    program_type: &str,
) -> Result<Option<u64>, StorageError> {
    match storage {
        Storage::Postgres(db) => Ok(db.get_committed_checkpoint_internal(program_type).await?),
        #[cfg(test)]
        Storage::Mock(mock_db) => mock_db.get_committed_checkpoint(program_type).await,
    }
}
