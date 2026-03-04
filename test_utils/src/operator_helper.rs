use {
    contra_indexer::{
        config::{ContraIndexerConfig, OperatorConfig, PostgresConfig, ProgramType},
        operator::{self},
        storage::{PostgresDb, Storage},
    },
    solana_sdk::{commitment_config::CommitmentLevel, pubkey::Pubkey, signature::Keypair},
    std::{sync::Arc, time::Duration},
    tokio::task::JoinHandle,
};

pub struct OperatorHandle {
    pub _handle: JoinHandle<()>,
}

impl OperatorHandle {
    pub async fn shutdown(self) {
        drop(self._handle);
    }
}

/// Start the operator that reads from L1 indexer and mints tokens on Contra
/// Returns the operator handle with graceful shutdown support
pub async fn start_l1_to_contra_operator(
    contra_rpc_url: String,
    l1_indexer_db_url: String,
    operator_keypair: Keypair,
    escrow_instance_id: Pubkey,
) -> Result<OperatorHandle, Box<dyn std::error::Error>> {
    // Initialize storage for L1 indexer database (where deposits are tracked)
    let storage = Arc::new(Storage::Postgres(
        PostgresDb::new(&PostgresConfig {
            database_url: l1_indexer_db_url.clone(),
            max_connections: 10,
        })
        .await?,
    ));

    // Common config for the operator
    let common_config = ContraIndexerConfig {
        program_type: ProgramType::Escrow, // Reading from Escrow program (deposits on L1)
        storage_type: contra_indexer::config::StorageType::Postgres,
        rpc_url: contra_rpc_url, // Contra RPC to send mint transactions to
        source_rpc_url: None,
        postgres: PostgresConfig {
            database_url: l1_indexer_db_url,
            max_connections: 10,
        },
        escrow_instance_id: Some(escrow_instance_id),
    };

    // Operator-specific config
    let operator_config = OperatorConfig {
        db_poll_interval: Duration::from_millis(500), // Poll every 500ms
        batch_size: 10,
        retry_max_attempts: 3,
        retry_base_delay: Duration::from_secs(1),
        channel_buffer_size: 100,
        rpc_commitment: CommitmentLevel::Confirmed,
        alert_webhook_url: None,
        reconciliation_interval: Duration::from_secs(5 * 60),
        reconciliation_tolerance_bps: 10,
        reconciliation_webhook_url: None,
        feepayer_monitor_interval: Duration::from_secs(60),
    };

    // Set up environment variables for the operator signer
    // Use the admin keypair as the signer (it's also the mint authority)
    let operator_private_key_base58 = bs58::encode(operator_keypair.to_bytes()).into_string();
    std::env::set_var("ADMIN_SIGNER", "memory");
    std::env::set_var("ADMIN_PRIVATE_KEY", &operator_private_key_base58);
    std::env::set_var("OPERATOR_SIGNER", "memory");
    std::env::set_var("OPERATOR_PRIVATE_KEY", &operator_private_key_base58);
    let task_handle = tokio::spawn(async move {
        if let Err(e) = operator::run(storage, common_config, operator_config).await {
            tracing::error!("Operator error: {}", e);
        }
    });

    Ok(OperatorHandle {
        _handle: task_handle,
    })
}

/// Start the operator that reads from L1 indexer and mints tokens on Contra
/// Returns the operator handle with graceful shutdown support
pub async fn start_contra_to_l1_operator(
    l1_rpc_url: String,
    contra_indexer_db_url: String,
    operator_keypair: Keypair,
    escrow_instance_id: Pubkey,
) -> Result<OperatorHandle, Box<dyn std::error::Error>> {
    // Initialize storage for L1 indexer database (where deposits are tracked)
    let storage = Arc::new(Storage::Postgres(
        PostgresDb::new(&PostgresConfig {
            database_url: contra_indexer_db_url.clone(),
            max_connections: 10,
        })
        .await?,
    ));

    // Common config for the operator
    let common_config = ContraIndexerConfig {
        program_type: ProgramType::Withdraw, // Reading from Withdraw program (releases on Contra)
        storage_type: contra_indexer::config::StorageType::Postgres,
        rpc_url: l1_rpc_url, // L1 RPC to send mint transactions to
        source_rpc_url: None,
        postgres: PostgresConfig {
            database_url: contra_indexer_db_url,
            max_connections: 10,
        },
        escrow_instance_id: Some(escrow_instance_id), // Needed for ReleaseFunds instructions
    };

    // Operator-specific config
    let operator_config = OperatorConfig {
        db_poll_interval: Duration::from_millis(500), // Poll every 500ms
        batch_size: 10,
        retry_max_attempts: 3,
        retry_base_delay: Duration::from_secs(1),
        channel_buffer_size: 100,
        rpc_commitment: CommitmentLevel::Confirmed,
        alert_webhook_url: None,
        reconciliation_interval: Duration::from_secs(5 * 60),
        reconciliation_tolerance_bps: 10,
        reconciliation_webhook_url: None,
        feepayer_monitor_interval: Duration::from_secs(60),
    };

    // Set up environment variables for the operator signer
    // Use the admin keypair as the signer (it's also the mint authority)
    let operator_private_key_base58 = bs58::encode(operator_keypair.to_bytes()).into_string();
    std::env::set_var("ADMIN_SIGNER", "memory");
    std::env::set_var("ADMIN_PRIVATE_KEY", &operator_private_key_base58);
    std::env::set_var("OPERATOR_SIGNER", "memory");
    std::env::set_var("OPERATOR_PRIVATE_KEY", &operator_private_key_base58);

    let task_handle = tokio::spawn(async move {
        if let Err(e) = operator::run(storage, common_config, operator_config).await {
            tracing::error!("Operator error: {}", e);
        }
    });

    Ok(OperatorHandle {
        _handle: task_handle,
    })
}
