use crate::{error::StorageError, storage::common::storage::Storage};

pub async fn drop_tables(storage: &Storage) -> Result<(), StorageError> {
    match storage {
        Storage::Postgres(db) => {
            db.drop_tables().await?;
            Ok(())
        }
        #[cfg(test)]
        Storage::Mock(mock_db) => mock_db.drop_tables().await,
    }
}
