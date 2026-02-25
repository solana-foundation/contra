use crate::{
    config::ProgramType,
    error::IndexerError,
    storage::Storage,
};
use std::sync::Arc;
use tracing::info;

/// Resync service for rebuilding indexer database from chain history
pub struct ResyncService {
    storage: Arc<Storage>,
    program_type: ProgramType,
}

impl ResyncService {
    pub fn new(storage: Arc<Storage>, program_type: ProgramType) -> Self {
        Self {
            storage,
            program_type,
        }
    }

    /// Run the resync process
    /// Returns Ok(()) if resync successful, Err otherwise
    pub async fn run(&self, genesis_slot: u64) -> Result<(), IndexerError> {
        info!(
            "Starting database resync for {:?} from slot {}...",
            self.program_type, genesis_slot
        );

        // TODO: Implement resync logic
        // 1. Drop existing tables
        // 2. Recreate schema
        // 3. Invoke backfill service from genesis_slot to current slot
        // 4. Process all transactions

        info!("Resync complete for {:?}", self.program_type);
        Ok(())
    }
}
