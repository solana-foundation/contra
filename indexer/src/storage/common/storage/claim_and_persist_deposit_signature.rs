use crate::{error::StorageError, storage::common::storage::Storage};

/// Atomic ownership claim + write-ahead signature persist for a deposit mint.
/// `Ok(Some(lease))` means the sender still owns the `Processing` incarnation
/// it was handed and may broadcast; the returned lease is the row's new
/// `updated_at`, which a later re-claim must present. `Ok(None)` means the row
/// was demoted or re-locked so the builder must be dropped without broadcasting.
pub async fn claim_and_persist_deposit_signature(
    storage: &Storage,
    transaction_id: i64,
    expected_updated_at: chrono::DateTime<chrono::Utc>,
    signature: String,
    last_valid_block_height: i64,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, StorageError> {
    match storage {
        Storage::Postgres(db) => Ok(db
            .claim_and_persist_deposit_signature_internal(
                transaction_id,
                expected_updated_at,
                signature,
                last_valid_block_height,
            )
            .await?),
        #[cfg(any(test, feature = "test-mock-storage"))]
        Storage::Mock(mock) => {
            mock.claim_and_persist_deposit_signature(
                transaction_id,
                expected_updated_at,
                signature,
                last_valid_block_height,
            )
            .await
        }
    }
}
