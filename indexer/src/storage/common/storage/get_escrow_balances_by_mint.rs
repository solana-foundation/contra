use crate::{
    error::StorageError,
    storage::common::{models::MintDbBalance, storage::Storage},
};

pub async fn get_escrow_balances_by_mint(
    storage: &Storage,
) -> Result<Vec<MintDbBalance>, StorageError> {
    match storage {
        Storage::Postgres(db) => Ok(db.get_escrow_balances_by_mint_internal().await?),
        #[cfg(test)]
        Storage::Mock(mock_db) => mock_db.get_escrow_balances_by_mint().await,
    }
}
