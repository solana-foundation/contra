use crate::{
    error::StorageError,
    storage::common::{models::DbMint, storage::Storage},
};

pub async fn get_mint(
    storage: &Storage,
    mint_address: &str,
) -> Result<Option<DbMint>, StorageError> {
    match storage {
        Storage::Postgres(db) => db.get_mint_internal(mint_address).await,
        #[cfg(test)]
        Storage::Mock(mock_db) => mock_db.get_mint(mint_address).await,
    }
}
