use crate::{
    error::StorageError,
    storage::common::{models::MintDbBalance, storage::Storage},
};

pub async fn get_mint_balances_for_reconciliation(
    storage: &Storage,
) -> Result<Vec<MintDbBalance>, StorageError> {
    match storage {
        Storage::Postgres(db) => Ok(db.get_mint_balances_for_reconciliation_internal().await?),
        #[cfg(any(test, feature = "test-mock-storage"))]
        Storage::Mock(mock) => mock.get_mint_balances_for_reconciliation().await,
    }
}
