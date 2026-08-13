use crate::{error::StorageError, storage::common::storage::Storage};

/// Record the tree generation the sender owes, before the rotation is dispatched.
pub async fn set_owed_rotation_target(
    storage: &Storage,
    program_type: &str,
    target_tree_index: u64,
) -> Result<(), StorageError> {
    match storage {
        Storage::Postgres(db) => Ok(db
            .set_owed_rotation_target_internal(program_type, target_tree_index)
            .await?),
        #[cfg(any(test, feature = "test-mock-storage"))]
        Storage::Mock(mock_db) => {
            mock_db
                .set_owed_rotation_target(program_type, target_tree_index)
                .await
        }
    }
}
