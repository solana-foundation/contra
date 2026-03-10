use crate::channel_utils::send_guaranteed;
use crate::error::account::AccountError;
use crate::error::OperatorError;
use crate::operator::sender::types::{PendingRemint, TransactionContext};
use crate::operator::tree_constants::MAX_TREE_LEAVES;
use crate::operator::utils::smt_util::SmtState;
use crate::operator::{MintCache, TransactionStatusUpdate, WithdrawalRemintInfo};
use crate::operator::{parse_instance, RetryConfig, RpcClientWithRetry};
use crate::storage::TransactionStatus;
use crate::storage::common::storage::Storage;
use crate::ContraIndexerConfig;
use chrono::Utc;
use solana_sdk::commitment_config::{CommitmentConfig, CommitmentLevel};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signature;
use spl_associated_token_account::get_associated_token_address_with_program_id;
use tokio::sync::mpsc;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tracing::{error, info};

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
            program_type: config.program_type,
            remint_cache: HashMap::new(),
            pending_signatures: HashMap::new(),
            pending_remints: Vec::new(),
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

        // CRITICAL: Verify local SMT root matches on-chain root
        // This ensures database is in sync with on-chain state
        let computed_root = smt_state.current_root();
        let onchain_root = instance.withdrawal_transactions_root;

        if computed_root != onchain_root {
            error!("SMT root mismatch detected! Database out of sync with on-chain state.");
            error!("  Instance PDA: {}", instance_pda);
            error!("  Tree Index: {}", tree_index);
            error!("  Nonces from DB: {:?}", nonces);
            error!("  Local root:    {:?}", computed_root);
            error!("  On-chain root: {:?}", onchain_root);
            error!("");
            error!("This typically means:");
            error!("  1. A withdrawal was successfully processed on-chain");
            error!("  2. But the operator crashed before updating the database");
            error!("  3. The database is now missing transaction records");
            error!("");
            error!("Resolution options:");
            error!("  1. Reset and resync the database from on-chain events");
            error!("  2. Manually reconcile missing transactions");
            error!("  3. Reset the on-chain SMT tree (requires admin)");

            return Err(crate::error::ProgramError::SmtRootMismatch {
                local_root: computed_root,
                onchain_root,
            }
            .into());
        }

        info!("SMT root verification passed: {:?}", computed_root);

        self.smt_state = Some(SenderSMTState {
            smt_state,
            nonce_to_builder: HashMap::new(),
        });

        Ok(())
    }

    /// Sends a ManualReview status update during startup recovery when a stored            
    /// transaction cannot be reconstructed (e.g. unparseable pubkey or signature).         
    /// Using send_guaranteed so the alert is never silently dropped.                       
    async fn send_recovery_manual_review(                                                   
        storage_tx: &mpsc::Sender<TransactionStatusUpdate>,                                 
        transaction_id: i64,                                                                
        trace_id: &str,                                   
        reason: &str,                                                                       
    ) {                                                   
        send_guaranteed(
            storage_tx,                                                                     
            TransactionStatusUpdate {
                transaction_id,                                                             
                trace_id: Some(trace_id.to_string()),     
                status: TransactionStatus::ManualReview,
                counterpart_signature: None,
                processed_at: Some(Utc::now()),                                             
                error_message: Some(format!("recovery failed: {}", reason)),                
                remint_signature: None,                                                     
            },                                                                              
            "transaction status update",                  
        )
        .await                                                                              
        .ok();
    }                                                                                                                                                    
                                                        
    pub(super) async fn recover_pending_remints(
        &mut self,                                                                          
        storage_tx: &mpsc::Sender<TransactionStatusUpdate>,
    ) -> Result<(), OperatorError> {                                                        
        let transactions = self.storage.get_pending_remint_transactions().await?;           
                                                                                            
        if transactions.is_empty() {                                                        
            return Ok(());                                                                  
        }                                                                                   
                                                            
        info!(
            "Recovering {} pending remint(s) from database",
            transactions.len()                                                              
        );
                                                                                            
        // Contra only supports SPL Token for now.                                          
        let contra_token_program = self.mint_cache.get_contra_token_program();
                                                                                            
        for tx in transactions {                          
            // Parse pubkeys stored as strings. On any failure we cannot remint safely,     
            // and silently skipping would leave the row stuck in PendingRemint on every    
            // restart — so we escalate to ManualReview.                                    
            let mint = match Pubkey::from_str(&tx.mint) {                                   
                Ok(pk) => pk,                                                               
                Err(e) => {                               
                    error!(transaction_id = tx.id, "Recovery: invalid mint pubkey: {}", e); 
                    Self::send_recovery_manual_review(
                        storage_tx, 
                        tx.id, 
                        &tx.trace_id, 
                        &format!("invalid mint pubkey: {}", e)
                    ).await;                                          
                    continue;
                }                                                                           
            };                                            

            let user = match Pubkey::from_str(&tx.recipient) {
                Ok(pk) => pk,                                                               
                Err(e) => {                               
                    error!(transaction_id = tx.id, "Recovery: invalid user pubkey: {}", e); 
                    Self::send_recovery_manual_review(
                        storage_tx, 
                        tx.id, 
                        &tx.trace_id,
                        &format!("invalid user pubkey: {}", e)
                    ).await;                                          
                    continue;                                                               
                }                                                                           
            };                                            

            let user_ata =
                get_associated_token_address_with_program_id(&user, &mint, &contra_token_program);                                          
                                                                                            
            // u64::try_from catches negative amounts. The write path already guards        
            // against this (ba77249) but a corrupt DB row could still produce one —
            // casting a negative i64 to u64 would produce a massive spurious remint.       
            let amount = match u64::try_from(tx.amount) {                                   
                Ok(a) => a,                                                                 
                Err(_) => {                                                                 
                    error!(transaction_id = tx.id, "Recovery: negative amount {}", tx.amount);                                                                             
                    Self::send_recovery_manual_review(
                        storage_tx, 
                        tx.id, 
                        &tx.trace_id,
                        &format!("negative amount {}", tx.amount)
                    ).await;                                       
                    continue;                             
                }                                                                           
            };
                                                                                            
            // Parse all stored withdrawal signatures. These are passed to                  
            // get_signature_statuses() by process_pending_remints to verify the
            // withdrawal did not finalize before we remint. A single bad entry             
            // means we cannot safely do that check — escalate to ManualReview.             
            let sig_strings = tx.remint_signatures.unwrap_or_default();                     
            let signatures = match sig_strings                                              
                .iter()                                                                     
                .map(|s| Signature::from_str(s))                                            
                .collect::<Result<Vec<_>, _>>()           
            {                                                                               
                Ok(sigs) => sigs,
                Err(e) => {                                                                 
                    error!(transaction_id = tx.id, "Recovery: invalid withdrawal signature: {}", e);
                    Self::send_recovery_manual_review(
                        storage_tx, 
                        tx.id, 
                        &tx.trace_id, 
                        &format!("invalid withdrawal signature: {}", e)
                    ).await;                                 
                    continue;
                }                                                                           
            };                                            

            // Restore the original deadline. Fall back to now() if missing (shouldn't      
            // happen) so the entry fires on the next tick instead of waiting 32s more.
            let deadline = tx.pending_remint_deadline_at.unwrap_or_else(Utc::now);          
                                                            
            let ctx = TransactionContext {                                                  
                transaction_id: Some(tx.id),              
                // Nonce is not needed for the remint — SMT cleanup already ran in          
                // handle_permanent_failure before the row was written as PendingRemint.    
                withdrawal_nonce: tx.withdrawal_nonce.map(|n| n as u64),                    
                trace_id: Some(tx.trace_id.clone()),                                        
            };                                                                              
                                                                                            
            let remint_info = WithdrawalRemintInfo {                                        
                transaction_id: tx.id,
                trace_id: tx.trace_id,                                                      
                mint,                                     
                user,
                user_ata,
                token_program: contra_token_program,
                amount,                                                                     
            };
                                                                                            
            info!(                                                                          
                transaction_id = tx.id,
                nonce = ctx.withdrawal_nonce.map(|n| n as i64),                             
                sigs = signatures.len(),                                                    
                "Recovered PendingRemint, deadline={}",
                deadline,                                                                   
            );                                            
                                                                                            
            self.pending_remints.push(PendingRemint {     
                ctx,
                remint_info,
                signatures,                                                                 
                // The original error string is not stored in DB. Only surfaced in
                // combined error messages if the remint itself also fails.                 
                original_error: "recovered from persistent storage".to_string(),            
                deadline,                                                                   
                finality_check_attempts: 0,                                                 
            });                                                                             
        }                                                 

        Ok(())
    }
}
