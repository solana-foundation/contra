use crate::{error::StorageError, storage::common::storage::Storage};

pub async fn set_mint_pausable(
    storage: &Storage,
    mint_address: &str,
    is_pausable: bool,
) -> Result<(), StorageError> {
    match storage {
        Storage::Postgres(db) => db.set_mint_pausable_internal(mint_address, is_pausable).await,
        #[cfg(test)]
        Storage::Mock(mock_db) => mock_db.set_mint_pausable(mint_address, is_pausable).await,
    }
}
