use crate::{error::StorageError, storage::common::storage::Storage};

pub async fn update_committed_checkpoint(
    storage: &Storage,
    program_type: &str,
    slot: u64,
) -> Result<(), StorageError> {
    match storage {
        Storage::Postgres(db) => Ok(db
            .update_committed_checkpoint_internal(program_type, slot)
            .await?),
        #[cfg(test)]
        Storage::Mock(mock_db) => {
            mock_db
                .update_committed_checkpoint(program_type, slot)
                .await
        }
    }
}
