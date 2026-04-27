use crate::{error::StorageError, storage::common::storage::Storage};

pub async fn set_mint_extension_flags(
    storage: &Storage,
    mint_address: &str,
    is_pausable: bool,
    has_permanent_delegate: bool,
) -> Result<(), StorageError> {
    match storage {
        Storage::Postgres(db) => {
            db.set_mint_extension_flags_internal(mint_address, is_pausable, has_permanent_delegate)
                .await
        }
        #[cfg(any(test, feature = "test-mock-storage"))]
        Storage::Mock(mock_db) => {
            mock_db
                .set_mint_extension_flags(mint_address, is_pausable, has_permanent_delegate)
                .await
        }
    }
}
