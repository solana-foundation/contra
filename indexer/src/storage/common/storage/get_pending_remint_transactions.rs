use crate::{
    error::StorageError,
    storage::common::{models::DbTransaction, storage::Storage},
};

pub async fn get_pending_remint_transactions(
    storage: &Storage,
) -> Result<Vec<DbTransaction>, StorageError> {
    match storage {
        Storage::Postgres(db) => {
            let pending_remints = db.get_pending_remint_transactions_internal().await?;

            Ok(pending_remints)
        }
        #[cfg(test)]
        Storage::Mock(mock_db) => mock_db.get_pending_remint_transactions().await,
    }
}
