use crate::error::account::AccountError;
use crate::error::OperatorError;
use crate::operator::tree_constants::MAX_TREE_LEAVES;
use crate::operator::utils::smt_util::SmtState;
use crate::operator::MintCache;
use crate::operator::{parse_instance, RetryConfig, RpcClientWithRetry};
use crate::storage::common::storage::Storage;
use crate::ContraIndexerConfig;
use solana_sdk::commitment_config::{CommitmentConfig, CommitmentLevel};
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::info;

use super::types::{SenderSMTState, SenderState};

impl SenderState {
    pub(super) fn new(
        config: &ContraIndexerConfig,
        operator_commitment: CommitmentLevel,
        instance_pda: Option<Pubkey>,
        storage: Arc<Storage>,
        retry_max_attempts: u32,
        source_rpc_client: Option<Arc<RpcClientWithRetry>>,
    ) -> Result<Self, OperatorError> {
        // Initialize global RPC client with retry
        let rpc_client = Arc::new(RpcClientWithRetry::with_retry_config(
            config.rpc_url.clone(),
            RetryConfig::default(),
            CommitmentConfig {
                commitment: operator_commitment,
            },
        ));

        let mint_rpc_client = source_rpc_client.unwrap_or_else(|| rpc_client.clone());
        let mint_cache = MintCache::with_rpc(storage.clone(), mint_rpc_client);

        Ok(Self {
            rpc_client,
            storage,
            instance_pda,
            smt_state: None,
            retry_counts: HashMap::new(),
            mint_cache,
            mint_builders: HashMap::new(),
            retry_max_attempts,
            rotation_retry_queue: Vec::new(),
            pending_rotation: None,
        })
    }

    /// Initialize SMT state lazily on first use
    /// Fetches tree_index from chain and populates SMT with completed withdrawals from DB
    pub(super) async fn initialize_smt_state(&mut self) -> Result<(), OperatorError> {
        let instance_pda = self
            .instance_pda
            .ok_or_else(|| AccountError::InstanceNotFound {
                instance: Pubkey::default(),
            })?;

        info!("Initializing SMT state for instance {}", instance_pda);

        let instance_data = self
            .rpc_client
            .get_account_data(&instance_pda)
            .await
            .map_err(|_| AccountError::AccountNotFound {
                pubkey: instance_pda,
            })?;

        let instance = parse_instance(&instance_data).map_err(|e| {
            AccountError::AccountDeserializationFailed {
                pubkey: instance_pda,
                reason: e.to_string(),
            }
        })?;

        let tree_index = instance.current_tree_index;
        let min_nonce = tree_index * MAX_TREE_LEAVES as u64;
        let max_nonce = (tree_index + 1) * MAX_TREE_LEAVES as u64;

        // Fetch completed withdrawal nonces for current tree from DB
        let nonces = self
            .storage
            .get_completed_withdrawal_nonces(min_nonce, max_nonce)
            .await?;

        // Create SMT and populate with existing nonces
        let mut smt_state = SmtState::new(tree_index);
        for nonce in &nonces {
            smt_state.insert_nonce(*nonce);
        }

        info!(
            "SMT state initialized with tree_index {}, populated {} existing nonces",
            tree_index,
            nonces.len()
        );

        self.smt_state = Some(SenderSMTState {
            smt_state,
            nonce_to_builder: HashMap::new(),
        });

        Ok(())
    }
}
