use crate::{
    error::StorageError,
    storage::{
        common::{models::MintDbBalance, storage::Storage},
        postgres::db::PostgresDb,
    },
};

/// Query escrow balances by mint for continuous reconciliation checks.
/// Unlike `get_mint_balances_for_reconciliation` (used at startup), this function
/// only counts **completed** transactions for both deposits and withdrawals,
/// providing a conservative view of what should be in the escrow based on
/// finalized database state.
pub async fn get_escrow_balances_by_mint(
    storage: &Storage,
) -> Result<Vec<MintDbBalance>, StorageError> {
    match storage {
        Storage::Postgres(postgres_db) => get_escrow_balances_by_mint_postgres(postgres_db).await,
        #[cfg(test)]
        Storage::Mock(mock_db) => mock_db.get_escrow_balances_by_mint().await,
    }
}

async fn get_escrow_balances_by_mint_postgres(
    db: &PostgresDb,
) -> Result<Vec<MintDbBalance>, StorageError> {
    Ok(db.get_escrow_balances_by_mint_internal().await?)
}
