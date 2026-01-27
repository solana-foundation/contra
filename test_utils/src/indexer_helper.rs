use {
    contra_indexer::{
        storage::{PostgresDb, Storage},
        BackfillConfig, ContraIndexerConfig, DatasourceType, IndexerConfig, PostgresConfig,
        ProgramType, RpcPollingConfig, StorageType, YellowstoneConfig,
    },
    solana_sdk::{commitment_config::CommitmentLevel, pubkey::Pubkey},
    solana_transaction_status::UiTransactionEncoding,
    std::sync::Arc,
    tokio::task::JoinHandle,
};

pub struct IndexerHandle {
    _handles: Vec<JoinHandle<()>>,
}

impl IndexerHandle {
    pub fn abort(&self) {
        for handle in &self._handles {
            handle.abort();
        }
    }
}

/// Start the Contra indexer
/// If geyser_endpoint is Some, uses Yellowstone datasource; otherwise uses RPC polling
/// Returns the indexer handle and storage instance
pub async fn start_contra_indexer(
    geyser_endpoint: Option<String>,
    rpc_url: String,
    database_url: String,
) -> Result<(IndexerHandle, Storage), Box<dyn std::error::Error>> {
    // Initialize storage first so we can return it for test assertions
    let storage = Arc::new(Storage::Postgres(
        PostgresDb::new(&PostgresConfig {
            database_url: database_url.clone(),
            max_connections: 50,
        })
        .await?,
    ));
    storage.init_schema().await?;

    // Build config structs similar to cli.rs
    let postgres_config = PostgresConfig {
        database_url,
        max_connections: 50,
    };

    let rpc_polling_config = RpcPollingConfig {
        poll_interval_ms: 200,
        error_retry_interval_ms: 1000,
        batch_size: 10,
        from_slot: Some(1),
        encoding: UiTransactionEncoding::Json,
        commitment: CommitmentLevel::Finalized,
    };

    // Configure yellowstone only if geyser endpoint is provided
    let (datasource_type, yellowstone_config) = if let Some(endpoint) = geyser_endpoint {
        (
            DatasourceType::Yellowstone,
            Some(YellowstoneConfig {
                endpoint,
                x_token: None, // not needed for local test validator
                commitment: "finalized".to_string(),
            }),
        )
    } else {
        (DatasourceType::RpcPolling, None)
    };

    let backfill_config = BackfillConfig {
        enabled: true,
        batch_size: 100,
        max_gap_slots: 100,
        exit_after_backfill: false,
        rpc_url: rpc_url.clone(),
        start_slot: None,
    };

    let common_config = ContraIndexerConfig {
        program_type: ProgramType::Withdraw,
        storage_type: StorageType::Postgres,
        postgres: postgres_config,
        rpc_url,
        source_rpc_url: None,
        escrow_instance_id: None,
    };

    let indexer_config = IndexerConfig {
        datasource_type,
        rpc_polling: Some(rpc_polling_config),
        yellowstone: yellowstone_config,
        backfill: backfill_config,
    };

    indexer_config.validate()?;
    common_config.validate()?;

    // Spawn contra_indexer::run in a background task
    let indexer_handle = tokio::spawn(async move {
        if let Err(e) = contra_indexer::run(common_config, indexer_config).await {
            eprintln!("Indexer error: {}", e);
        }
    });

    // Give indexer time to initialize
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Clone storage for test assertions (the Arc makes this cheap)
    let storage_for_tests = (*storage).clone();

    Ok((
        IndexerHandle {
            _handles: vec![indexer_handle],
        },
        storage_for_tests,
    ))
}

/// Start the L1 indexer using Yellowstone geyser
/// Returns the indexer handle, storage instance, and the postgres container
pub async fn start_l1_indexer(
    geyser_endpoint: String,
    rpc_url: String,
    database_url: String,
    escrow_instance_id: Option<Pubkey>,
) -> Result<(IndexerHandle, Storage), Box<dyn std::error::Error>> {
    // Initialize storage first so we can return it for test assertions
    let storage = Arc::new(Storage::Postgres(
        PostgresDb::new(&PostgresConfig {
            database_url: database_url.clone(),
            max_connections: 50,
        })
        .await?,
    ));
    storage.init_schema().await?;

    // Build config structs similar to cli.rs
    let postgres_config = PostgresConfig {
        database_url: database_url.clone(),
        max_connections: 50,
    };

    let yellowstone_config = YellowstoneConfig {
        endpoint: geyser_endpoint,
        x_token: None, // not needed for local test validator
        commitment: "finalized".to_string(),
    };

    // RPC polling config is needed for backfill even when using Yellowstone
    let rpc_polling_config = RpcPollingConfig {
        poll_interval_ms: 200,
        error_retry_interval_ms: 1000,
        batch_size: 10,
        from_slot: Some(1),
        encoding: UiTransactionEncoding::Json,
        commitment: CommitmentLevel::Finalized,
    };

    let backfill_config = BackfillConfig {
        enabled: true,
        batch_size: 100,
        max_gap_slots: 100,
        exit_after_backfill: false,
        rpc_url: rpc_url.clone(),
        start_slot: None,
    };

    let common_config = ContraIndexerConfig {
        program_type: ProgramType::Escrow,
        storage_type: StorageType::Postgres,
        postgres: postgres_config,
        rpc_url,
        source_rpc_url: None,
        escrow_instance_id,
    };

    let indexer_config = IndexerConfig {
        datasource_type: DatasourceType::Yellowstone,
        rpc_polling: Some(rpc_polling_config),
        yellowstone: Some(yellowstone_config),
        backfill: backfill_config,
    };

    common_config.validate()?;
    indexer_config.validate()?;

    // Spawn contra_indexer::run in a background task
    let indexer_handle = tokio::spawn(async move {
        if let Err(e) = contra_indexer::run(common_config, indexer_config).await {
            eprintln!("L1 Indexer error: {}", e);
        }
    });

    // Give indexer time to initialize
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Clone storage for test assertions (the Arc makes this cheap)
    let storage_for_tests = (*storage).clone();

    Ok((
        IndexerHandle {
            _handles: vec![indexer_handle],
        },
        storage_for_tests,
    ))
}
