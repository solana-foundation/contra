use crate::{error::StorageError, storage::common::storage::Storage};

pub async fn close(storage: &Storage) -> Result<(), StorageError> {
    match storage {
        Storage::Postgres(db) => {
            db.close().await?;
            Ok(())
        }
        #[cfg(test)]
        Storage::Mock(mock_db) => mock_db.close().await,
    }
}
