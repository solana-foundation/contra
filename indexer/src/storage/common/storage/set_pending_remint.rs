use crate::{
    error::StorageError,
    storage::{common::storage::Storage, postgres::db::PostgresDb},
};

pub async fn set_pending_remint(
    storage: &Storage,
    transaction_id: i64,
    remint_signatures: Vec<String>,
    deadline_at: chrono::DateTime<chrono::Utc>,
) -> Result<(), StorageError> {
    match storage {
        Storage::Postgres(postgres_db) => {
            set_pending_remint_postgres(postgres_db, transaction_id, remint_signatures, deadline_at)
                .await
        }
        #[cfg(test)]
        Storage::Mock(mock_db) => {
            mock_db
                .set_pending_remint(transaction_id, remint_signatures, deadline_at)
                .await
        }
    }
}

async fn set_pending_remint_postgres(
    db: &PostgresDb,
    transaction_id: i64,
    remint_signatures: Vec<String>,
    deadline_at: chrono::DateTime<chrono::Utc>,
) -> Result<(), StorageError> {
    db.set_pending_remint_internal(transaction_id, remint_signatures, deadline_at)
        .await?;
    Ok(())
}
