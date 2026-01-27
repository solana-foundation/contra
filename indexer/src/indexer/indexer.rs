use crate::error::{DataSourceError, IndexerError};
use crate::{
    indexer::{
        checkpoint::CheckpointWriter, datasource::common::datasource::DataSource,
        transaction_processor::TransactionProcessor,
    },
    shutdown_utils::{cleanup_after_backfill, shutdown_indexer},
    storage::{PostgresDb, Storage},
    ContraIndexerConfig, DatasourceType, IndexerConfig, StorageType,
};

#[cfg(feature = "datasource-rpc")]
use crate::indexer::backfill::BackfillService;

#[cfg(feature = "datasource-rpc")]
use crate::indexer::datasource::rpc_polling::{rpc::RpcPoller, RpcPollingSource};

#[cfg(feature = "datasource-yellowstone")]
use crate::indexer::datasource::yellowstone::YellowstoneSource;
use std::sync::Arc;
use tokio::signal;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

pub async fn run(
    common_config: ContraIndexerConfig,
    indexer_config: IndexerConfig,
) -> Result<(), IndexerError> {
    info!("Starting Contra Indexer");
    info!("Program: {:?}", common_config.program_type);
    info!("Datasource: {:?}", indexer_config.datasource_type);
    info!("Storage: {:?}", common_config.storage_type);
    info!("RPC URL: {}", common_config.rpc_url);
    info!("Backfill enabled: {}", indexer_config.backfill.enabled);

    // 1. Initialize storage
    let storage: Arc<Storage> = match common_config.storage_type {
        StorageType::Postgres => Arc::new(Storage::Postgres(
            PostgresDb::new(&common_config.postgres)
                .await
                .map_err(|e| IndexerError::Storage(e.into()))?,
        )),
    };
    storage.init_schema().await?;
    info!("Storage initialized");

    // 2. Create channels
    let (instruction_tx, instruction_rx) = mpsc::channel(1000);
    let (checkpoint_tx, checkpoint_rx) = mpsc::channel(1000);

    // 3. Start checkpoint writer service
    let checkpoint_writer = CheckpointWriter::new(storage.clone());
    let checkpoint_handle = checkpoint_writer.start(checkpoint_rx);
    info!("CheckpointWriter service started");

    // 4. Run backfill if enabled
    if indexer_config.backfill.enabled {
        #[cfg(not(feature = "datasource-rpc"))]
        return Err(DataSourceError::InvalidConfig {
            reason: "Datasource rpc needs to be enabled for backfilling".to_string(),
        });

        #[cfg(feature = "datasource-rpc")]
        {
            use crate::error::DataSourceError;

            let rpc_polling_config = indexer_config.rpc_polling.as_ref().ok_or_else(|| {
                DataSourceError::InvalidConfig {
                    reason: "RPC polling config required for backfill".to_string(),
                }
            })?;
            let rpc_poller = Arc::new(RpcPoller::new(
                indexer_config.backfill.rpc_url.clone(),
                rpc_polling_config.encoding,
                rpc_polling_config.commitment,
            ));

            let backfill_service = BackfillService::new(
                storage.clone(),
                rpc_poller,
                common_config.program_type,
                indexer_config.backfill.clone(),
                common_config.escrow_instance_id,
            );

            if indexer_config.backfill.exit_after_backfill {
                // Run backfill synchronously if exiting after
                backfill_service.run(instruction_tx.clone()).await?;
                info!("Backfill completed, performing graceful cleanup...");
                if let Err(e) =
                    cleanup_after_backfill(checkpoint_handle, checkpoint_tx, storage).await
                {
                    error!("Cleanup after backfill failed: {}", e);
                    return Err(IndexerError::ShutdownChannelSend);
                }
                return Ok(());
            } else {
                // Run backfill concurrently with live indexing
                let instruction_tx_clone = instruction_tx.clone();
                tokio::spawn(async move {
                    if let Err(e) = backfill_service.run(instruction_tx_clone).await {
                        error!("Backfill failed: {}", e);
                    } else {
                        info!("Backfill completed successfully");
                    }
                });
            }
        }
    }

    // 5. Start datasource
    let mut datasource: Box<dyn DataSource> = match indexer_config.datasource_type {
        #[cfg(feature = "datasource-rpc")]
        DatasourceType::RpcPolling => {
            let rpc_config = indexer_config.rpc_polling.as_ref().ok_or_else(|| {
                DataSourceError::InvalidConfig {
                    reason: "RPC polling config required for RpcPolling datasource".to_string(),
                }
            })?;

            Box::new(RpcPollingSource::new(
                common_config.rpc_url.clone(),
                rpc_config.from_slot,
                rpc_config.poll_interval_ms,
                rpc_config.error_retry_interval_ms,
                rpc_config.batch_size,
                rpc_config.encoding,
                rpc_config.commitment,
                common_config.program_type,
                common_config.escrow_instance_id,
            ))
        }

        #[cfg(feature = "datasource-yellowstone")]
        DatasourceType::Yellowstone => {
            let yellowstone_config = indexer_config.yellowstone.as_ref().ok_or_else(|| {
                DataSourceError::InvalidConfig {
                    reason: "Yellowstone config required for Yellowstone datasource".to_string(),
                }
            })?;

            info!(
                "Starting Yellowstone datasource from {} (commitment: {})",
                yellowstone_config.endpoint, yellowstone_config.commitment
            );

            Box::new(YellowstoneSource::new(
                yellowstone_config.endpoint.clone(),
                yellowstone_config.x_token.clone(),
                yellowstone_config.commitment.clone(),
                common_config.program_type,
                common_config.escrow_instance_id,
            ))
        }

        // Catch-all for disabled features
        #[allow(unreachable_patterns)]
        _ => {
            return Err(DataSourceError::InvalidConfig {
                reason: format!(
                    "Datasource {:?} is not compiled. Rebuild with the appropriate feature flag",
                    indexer_config.datasource_type
                ),
            }
            .into());
        }
    };

    // 6. Create cancellation token for graceful shutdown
    let cancellation_token = CancellationToken::new();

    info!("Starting datasource...");
    let datasource_handle = datasource
        .start(instruction_tx.clone(), cancellation_token.clone())
        .await?;

    // 6. Start transaction processor
    let transaction_processor = TransactionProcessor::new(storage.clone(), checkpoint_tx.clone());
    let processor_handle = tokio::spawn(async move {
        if let Err(e) = transaction_processor.start(instruction_rx).await {
            error!("TransactionProcessor error: {}", e);
        }
    });

    info!("Indexer started, waiting for shutdown signal...");

    // 7. Wait for shutdown signal
    signal::ctrl_c()
        .await
        .map_err(|_| IndexerError::ShutdownChannelSend)?;
    info!("Shutdown signal received, initiating graceful shutdown...");

    // 8. Graceful shutdown
    shutdown_indexer(
        cancellation_token,
        storage,
        datasource,
        datasource_handle,
        instruction_tx,
        checkpoint_tx,
        checkpoint_handle,
        processor_handle,
    )
    .await
    .map_err(|_| IndexerError::ShutdownChannelSend)?;

    info!("Indexer shutdown complete");
    Ok(())
}
