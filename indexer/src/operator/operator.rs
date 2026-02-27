use crate::config::OperatorConfig;
use crate::error::OperatorError;
use crate::operator::{
    fetcher, processor, sender, DbTransactionWriter, RetryConfig, RpcClientWithRetry,
};
use crate::shutdown_utils::shutdown_operator;
use crate::storage::Storage;
use crate::ContraIndexerConfig;
use solana_sdk::commitment_config::CommitmentConfig;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::info;

pub async fn run(
    storage: Arc<Storage>,
    common_config: ContraIndexerConfig,
    config: OperatorConfig,
) -> Result<(), OperatorError> {
    info!("Starting Contra Operator");
    info!("Program: {:?}", common_config.program_type);
    info!("Poll interval: {:?}", config.db_poll_interval);
    info!("Retry max attempts: {}", config.retry_max_attempts);

    let cancellation_token = CancellationToken::new();

    // Initialize global RPC client with retry
    let rpc_client = Arc::new(RpcClientWithRetry::with_retry_config(
        common_config.rpc_url.clone(),
        RetryConfig::default(),
        CommitmentConfig {
            commitment: config.rpc_commitment,
        },
    ));

    // Initialize source RPC client if configured
    let source_rpc_client = common_config.source_rpc_url.as_ref().map(|url| {
        Arc::new(RpcClientWithRetry::with_retry_config(
            url.clone(),
            RetryConfig::default(),
            CommitmentConfig {
                commitment: config.rpc_commitment,
            },
        ))
    });

    let (processor_tx, processor_rx) = mpsc::channel(config.channel_buffer_size);
    let (sender_tx, sender_rx) = mpsc::channel(config.channel_buffer_size);
    let (storage_tx, storage_rx) = mpsc::channel::<sender::TransactionStatusUpdate>(100);

    // Start fetcher task
    let fetcher_storage = storage.clone();
    let fetcher_config = config.clone();
    let fetcher_token = cancellation_token.clone();
    let fetcher_handle = tokio::spawn(async move {
        if let Err(e) = fetcher::run_fetcher(
            fetcher_storage,
            processor_tx,
            fetcher_config,
            common_config.program_type,
            fetcher_token,
        )
        .await
        {
            tracing::error!("Fetcher error: {}", e);
        }
    });

    // Start processor task
    let program_type = common_config.program_type;
    let instance_pda = common_config.escrow_instance_id;
    let processor_storage = storage.clone();
    let processor_rpc = rpc_client.clone();
    let processor_source_rpc = source_rpc_client.clone();
    let processor_handle = tokio::spawn(async move {
        processor::run_processor(
            processor_rx,
            sender_tx,
            program_type,
            instance_pda,
            processor_storage,
            processor_rpc,
            processor_source_rpc,
        )
        .await;
    });

    // Start storage writer task (receives updates from sender)
    let writer_storage = storage.clone();
    let storage_writer =
        DbTransactionWriter::new(writer_storage, storage_rx, config.alert_webhook_url.clone());
    let storage_writer_handle = tokio::spawn(async move {
        if let Err(e) = storage_writer.start().await {
            tracing::error!("Storage writer error: {}", e);
        }
    });

    // Start sender task
    let sender_token = cancellation_token.clone();
    let sender_storage = storage.clone();
    let sender_commitment = config.rpc_commitment;
    let sender_source_rpc = source_rpc_client.clone();
    let sender_handle = tokio::spawn(async move {
        if let Err(e) = sender::run_sender(
            &common_config,
            sender_commitment,
            sender_rx,
            storage_tx,
            sender_token,
            sender_storage,
            config.retry_max_attempts,
            sender_source_rpc,
        )
        .await
        {
            tracing::error!("Sender error: {}", e);
        }
    });

    info!("Operator started, waiting for shutdown signal...");

    // Wait for shutdown signal
    tokio::signal::ctrl_c()
        .await
        .map_err(|_| OperatorError::ShutdownChannelSend)?;
    info!("Shutdown signal received, initiating graceful shutdown...");

    // Graceful shutdown
    shutdown_operator(
        cancellation_token,
        storage,
        fetcher_handle,
        processor_handle,
        sender_handle,
        storage_writer_handle,
        config.batch_size,
        config.db_poll_interval,
    )
    .await
    .map_err(|_| OperatorError::ShutdownChannelSend)?;

    info!("Operator shutdown complete");
    Ok(())
}
