use crate::{error::StorageError, storage::common::storage::Storage};

/// Tree generation the sender still owes the chain, or `None` if none is owed.
pub async fn get_owed_rotation_target(
    storage: &Storage,
    program_type: &str,
) -> Result<Option<u64>, StorageError> {
    match storage {
        Storage::Postgres(db) => Ok(db.get_owed_rotation_target_internal(program_type).await?),
        #[cfg(any(test, feature = "test-mock-storage"))]
        Storage::Mock(mock_db) => mock_db.get_owed_rotation_target(program_type).await,
    }
}
