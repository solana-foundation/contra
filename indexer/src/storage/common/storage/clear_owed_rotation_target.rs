use crate::{error::StorageError, storage::common::storage::Storage};

/// Retire the owed rotation once a chain read proved `target_tree_index` landed.
pub async fn clear_owed_rotation_target(
    storage: &Storage,
    program_type: &str,
    target_tree_index: u64,
) -> Result<(), StorageError> {
    match storage {
        Storage::Postgres(db) => Ok(db
            .clear_owed_rotation_target_internal(program_type, target_tree_index)
            .await?),
        #[cfg(any(test, feature = "test-mock-storage"))]
        Storage::Mock(mock_db) => {
            mock_db
                .clear_owed_rotation_target(program_type, target_tree_index)
                .await
        }
    }
}
