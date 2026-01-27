use crate::{
    error::StorageError,
    storage::{
        common::{models::DbMint, storage::Storage},
        postgres::db::PostgresDb,
    },
};

pub async fn upsert_mints_batch(storage: &Storage, mints: &[DbMint]) -> Result<(), StorageError> {
    match storage {
        Storage::Postgres(postgres_db) => upsert_mints_batch_postgres(postgres_db, mints).await,
        #[cfg(test)]
        Storage::Mock(mock_db) => mock_db.upsert_mints_batch(mints).await,
    }
}

async fn upsert_mints_batch_postgres(
    db: &PostgresDb,
    mints: &[DbMint],
) -> Result<(), StorageError> {
    db.upsert_mints_batch_internal(mints).await?;
    Ok(())
}
