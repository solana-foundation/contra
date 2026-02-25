use crate::{
    error::StorageError,
    storage::{common::models::MintDbBalance, common::storage::Storage, postgres::db::PostgresDb},
};

pub async fn get_mint_balances_for_reconciliation(
    storage: &Storage,
) -> Result<Vec<MintDbBalance>, StorageError> {
    match storage {
        Storage::Postgres(db) => get_mint_balances_postgres(db).await,
        #[cfg(test)]
        Storage::Mock(mock) => mock.get_mint_balances_for_reconciliation().await,
    }
}

async fn get_mint_balances_postgres(db: &PostgresDb) -> Result<Vec<MintDbBalance>, StorageError> {
    Ok(db.get_mint_balances_for_reconciliation_internal().await?)
}
