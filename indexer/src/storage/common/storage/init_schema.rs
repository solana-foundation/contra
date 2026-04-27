use crate::{error::StorageError, storage::common::storage::Storage};

pub async fn init_schema(storage: &Storage) -> Result<(), StorageError> {
    match storage {
        Storage::Postgres(db) => {
            db.init_schema().await?;
            Ok(())
        }
        #[cfg(any(test, feature = "test-mock-storage"))]
        Storage::Mock(mock_db) => mock_db.init_schema().await,
    }
}
