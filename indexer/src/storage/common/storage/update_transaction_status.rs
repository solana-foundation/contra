use crate::{
    error::StorageError,
    storage::common::{models::TransactionStatus, storage::Storage},
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
        Storage::Postgres(db) => {
            db.update_transaction_status_internal(
                transaction_id,
                status,
                counterpart_signature,
                processed_at,
            )
            .await?;
            Ok(())
        }
        #[cfg(any(test, feature = "test-mock-storage"))]
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
