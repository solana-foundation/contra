use crate::{
    error::StorageError,
    storage::common::{models::DbMint, storage::Storage},
};

pub async fn upsert_mints_batch(storage: &Storage, mints: &[DbMint]) -> Result<(), StorageError> {
    match storage {
        Storage::Postgres(db) => {
            db.upsert_mints_batch_internal(mints).await?;
            Ok(())
        }
        #[cfg(any(test, feature = "test-mock-storage"))]
        Storage::Mock(mock_db) => mock_db.upsert_mints_batch(mints).await,
    }
}
