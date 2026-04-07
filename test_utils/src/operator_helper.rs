use {
    contra_indexer::{
        config::{ContraIndexerConfig, OperatorConfig, PostgresConfig, ProgramType, StorageType},
        operator,
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

fn default_operator_config() -> OperatorConfig {
    OperatorConfig {
        db_poll_interval: Duration::from_millis(500),
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
        confirmation_poll_interval_ms: 400,
    }
}

fn set_operator_env_vars(keypair: &Keypair) {
    let private_key_base58 = bs58::encode(keypair.to_bytes()).into_string();
    std::env::set_var("ADMIN_SIGNER", "memory");
    std::env::set_var("ADMIN_PRIVATE_KEY", &private_key_base58);
    std::env::set_var("OPERATOR_SIGNER", "memory");
    std::env::set_var("OPERATOR_PRIVATE_KEY", &private_key_base58);
}

/// Start the operator that reads from Solana indexer and mints tokens on Contra.
pub async fn start_solana_to_contra_operator(
    contra_rpc_url: String,
    solana_indexer_db_url: String,
    operator_keypair: Keypair,
    escrow_instance_id: Pubkey,
) -> Result<OperatorHandle, Box<dyn std::error::Error>> {
    let postgres_config = PostgresConfig {
        database_url: solana_indexer_db_url,
        max_connections: 10,
    };

    let storage = Arc::new(Storage::Postgres(PostgresDb::new(&postgres_config).await?));

    let common_config = ContraIndexerConfig {
        program_type: ProgramType::Escrow,
        storage_type: StorageType::Postgres,
        rpc_url: contra_rpc_url,
        source_rpc_url: None,
        postgres: postgres_config,
        escrow_instance_id: Some(escrow_instance_id),
    };

    let operator_config = default_operator_config();

    set_operator_env_vars(&operator_keypair);

    let task_handle = tokio::spawn(async move {
        if let Err(e) = operator::run(storage, common_config, operator_config).await {
            tracing::error!("Operator error: {}", e);
        }
    });

    Ok(OperatorHandle {
        _handle: task_handle,
    })
}

/// Start the operator that reads from Contra indexer and releases funds on Solana.
pub async fn start_contra_to_solana_operator(
    solana_rpc_url: String,
    contra_indexer_db_url: String,
    operator_keypair: Keypair,
    escrow_instance_id: Pubkey,
) -> Result<OperatorHandle, Box<dyn std::error::Error>> {
    let postgres_config = PostgresConfig {
        database_url: contra_indexer_db_url,
        max_connections: 10,
    };

    let storage = Arc::new(Storage::Postgres(PostgresDb::new(&postgres_config).await?));

    let common_config = ContraIndexerConfig {
        program_type: ProgramType::Withdraw,
        storage_type: StorageType::Postgres,
        rpc_url: solana_rpc_url,
        source_rpc_url: None,
        postgres: postgres_config,
        escrow_instance_id: Some(escrow_instance_id),
    };

    let operator_config = default_operator_config();

    set_operator_env_vars(&operator_keypair);

    let task_handle = tokio::spawn(async move {
        if let Err(e) = operator::run(storage, common_config, operator_config).await {
            tracing::error!("Operator error: {}", e);
        }
    });

    Ok(OperatorHandle {
        _handle: task_handle,
    })
}
