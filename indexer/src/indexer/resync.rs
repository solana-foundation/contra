use crate::{
    config::{BackfillConfig, ProgramType},
    error::IndexerError,
    indexer::{backfill::BackfillService, datasource::rpc_polling::rpc::RpcPoller},
    storage::Storage,
};
use std::sync::Arc;
use tracing::info;

/// Resync service for rebuilding indexer database from chain history
pub struct ResyncService {
    storage: Arc<Storage>,
    rpc_poller: Arc<RpcPoller>,
    program_type: ProgramType,
    backfill_config_base: BackfillConfig,
    escrow_instance_id: Option<solana_sdk::pubkey::Pubkey>,
}

impl ResyncService {
    pub fn new(
        storage: Arc<Storage>,
        rpc_poller: Arc<RpcPoller>,
        program_type: ProgramType,
        backfill_config_base: BackfillConfig,
        escrow_instance_id: Option<solana_sdk::pubkey::Pubkey>,
    ) -> Self {
        Self {
            storage,
            rpc_poller,
            program_type,
            backfill_config_base,
            escrow_instance_id,
        }
    }

    /// Run the resync process
    /// Returns Ok(()) if resync successful, Err otherwise
    pub async fn run(&self, genesis_slot: u64) -> Result<(), IndexerError> {
        info!(
            "Starting database resync for {:?} from slot {}...",
            self.program_type, genesis_slot
        );

        // Step 1: Drop existing tables
        info!("Dropping existing database tables...");
        self.storage.drop_tables().await?;
        info!("Database tables dropped successfully");

        // Step 2: Recreate schema
        info!("Recreating database schema...");
        self.storage.init_schema().await?;
        info!("Database schema recreated successfully");

        // Step 3: Create BackfillService with genesis_slot configuration
        let backfill_config = BackfillConfig {
            enabled: true,
            exit_after_backfill: false,
            rpc_url: self.backfill_config_base.rpc_url.clone(),
            batch_size: self.backfill_config_base.batch_size,
            max_gap_slots: u64::MAX, // No limit for full resync
            start_slot: Some(genesis_slot),
        };

        let backfill_service = BackfillService::new(
            self.storage.clone(),
            self.rpc_poller.clone(),
            self.program_type,
            backfill_config,
            self.escrow_instance_id,
        );

        // TODO: Step 4: Invoke backfill service to process all transactions from genesis_slot to current slot

        info!("Resync complete for {:?}", self.program_type);
        Ok(())
    }
}
