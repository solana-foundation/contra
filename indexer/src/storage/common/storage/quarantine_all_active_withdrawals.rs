use crate::{error::StorageError, storage::common::storage::Storage};

/// Mark every `Pending`/`Processing` withdrawal as `ManualReview`.
///
/// Called once per poison-pill detection in the withdrawal pipeline.
/// Halting the whole pipeline (rather than skipping the single bad row) is
/// the deliberate conservative choice: a quarantined withdrawal leaves a
/// permanent nonce gap that the on-chain program rejects, so continuing to
/// drain subsequent rows would produce a stream of on-chain failures until
/// a human intervenes. Stopping immediately concentrates the blast radius
/// into one actionable batch of `ManualReview` alerts and blocks the
/// fetcher from pulling further rows (no `Pending` left to fetch).
///
/// `exclude_id` is the poison row already quarantined via the async storage
/// writer — excluding it here prevents a duplicate `ManualReview` webhook if
/// the async update has not yet committed.
///
/// Terminal statuses (Completed, Failed, ManualReview, PendingRemint) are
/// left alone so operators don't get re-alerted on already-handled rows.
pub async fn quarantine_all_active_withdrawals(
    storage: &Storage,
    exclude_id: Option<i64>,
) -> Result<u64, StorageError> {
    match storage {
        Storage::Postgres(db) => db
            .quarantine_all_active_withdrawals_internal(exclude_id)
            .await
            .map_err(StorageError::from),
        #[cfg(test)]
        Storage::Mock(mock_db) => mock_db.quarantine_all_active_withdrawals(exclude_id).await,
    }
}
