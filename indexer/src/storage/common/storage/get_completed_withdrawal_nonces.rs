use crate::error::StorageError;
use crate::storage::common::storage::Storage;

pub async fn get_completed_withdrawal_nonces(
    storage: &Storage,
    min_nonce: u64,
    max_nonce: u64,
) -> Result<Vec<u64>, StorageError> {
    match storage {
        Storage::Postgres(db) => {
            let nonces = db
                .get_completed_withdrawal_nonces_internal(min_nonce as i64, max_nonce as i64)
                .await?;
            Ok(nonces.into_iter().map(|n| n as u64).collect())
        }
        #[cfg(test)]
        Storage::Mock(mock) => mock.get_completed_withdrawal_nonces(min_nonce, max_nonce),
    }
}
