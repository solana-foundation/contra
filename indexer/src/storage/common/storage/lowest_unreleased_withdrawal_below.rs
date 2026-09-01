use crate::{error::StorageError, storage::common::storage::Storage};

/// Lowest withdrawal nonce below `nonce` that still owes a release, or `None` if every
/// lower nonce is terminal. Gates the sender's tree-rotation submit.
pub async fn lowest_unreleased_withdrawal_below(
    storage: &Storage,
    nonce: i64,
) -> Result<Option<i64>, StorageError> {
    match storage {
        Storage::Postgres(db) => Ok(db
            .lowest_unreleased_withdrawal_below_internal(nonce)
            .await?),
        #[cfg(any(test, feature = "test-mock-storage"))]
        Storage::Mock(mock_db) => mock_db.lowest_unreleased_withdrawal_below(nonce).await,
    }
}
