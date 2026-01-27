use crate::{
    error::StorageError,
    storage::{
        common::{models::DbMint, storage::Storage},
        postgres::db::PostgresDb,
    },
};

pub async fn get_mint(
    storage: &Storage,
    mint_address: &str,
) -> Result<Option<DbMint>, StorageError> {
    match storage {
        Storage::Postgres(postgres_db) => get_mint_postgres(postgres_db, mint_address).await,
        #[cfg(test)]
        Storage::Mock(mock_db) => mock_db.get_mint(mint_address).await,
    }
}

async fn get_mint_postgres(
    db: &PostgresDb,
    mint_address: &str,
) -> Result<Option<DbMint>, StorageError> {
    db.get_mint_internal(mint_address).await
}
