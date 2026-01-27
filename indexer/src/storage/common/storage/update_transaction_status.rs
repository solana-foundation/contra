use crate::{
    error::StorageError,
    storage::{
        common::{models::TransactionStatus, storage::Storage},
        postgres::db::PostgresDb,
    },
};
use chrono::{DateTime, Utc};

pub async fn update_transaction_status(
    storage: &Storage,
    transaction_id: i64,
    status: TransactionStatus,
    counterpart_signature: Option<String>,
    processed_at: DateTime<Utc>,
) -> Result<(), StorageError> {
    match storage {
        Storage::Postgres(postgres_db) => {
            update_transaction_status_postgres(
                postgres_db,
                transaction_id,
                status,
                counterpart_signature,
                processed_at,
            )
            .await
        }
        #[cfg(test)]
        Storage::Mock(mock_db) => {
            mock_db
                .update_transaction_status(
                    transaction_id,
                    status,
                    counterpart_signature,
                    processed_at,
                )
                .await
        }
    }
}

async fn update_transaction_status_postgres(
    db: &PostgresDb,
    transaction_id: i64,
    status: TransactionStatus,
    counterpart_signature: Option<String>,
    processed_at: DateTime<Utc>,
) -> Result<(), StorageError> {
    db.update_transaction_status_internal(
        transaction_id,
        status,
        counterpart_signature,
        processed_at,
    )
    .await?;
    Ok(())
}
