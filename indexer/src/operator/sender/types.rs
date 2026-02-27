use crate::operator::RpcClientWithRetry;
use crate::storage::common::models::TransactionStatus;
use crate::storage::common::storage::Storage;
use crate::{operator::utils::smt_util::SmtState, operator::MintCache};
use chrono::{DateTime, Utc};
use contra_escrow_program_client::instructions::{ReleaseFundsBuilder, ResetSmtRootBuilder};
use solana_keychain::Signer;
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::sync::Arc;

use crate::operator::utils::instruction_util::MintToBuilder;

#[derive(Clone, Debug)]
pub struct TransactionContext {
    pub transaction_id: Option<i64>,
    pub withdrawal_nonce: Option<u64>,
    pub trace_id: Option<String>,
}

/// Transaction status update to send to storage
#[derive(Debug, Clone)]
pub struct TransactionStatusUpdate {
    pub transaction_id: i64,
    pub trace_id: Option<String>,
    pub status: TransactionStatus,
    pub counterpart_signature: Option<String>,
    pub processed_at: Option<DateTime<Utc>>,
    pub error_message: Option<String>,
}

/// Sender state tracking SMT and pending transactions
pub struct SenderState {
    pub rpc_client: Arc<RpcClientWithRetry>,
    pub storage: Arc<Storage>,
    pub instance_pda: Option<Pubkey>,
    pub smt_state: Option<SenderSMTState>,
    pub retry_counts: HashMap<u64, u32>,
    pub mint_builders: HashMap<i64, MintToBuilder>,
    pub mint_cache: MintCache,
    pub retry_max_attempts: u32,
    pub rotation_retry_queue: Vec<(TransactionContext, ReleaseFundsBuilder)>,
    /// Pending ResetSmtRoot transaction waiting for in-flight txs to settle
    pub pending_rotation: Option<Box<ResetSmtRootBuilder>>,
}

pub struct SenderSMTState {
    pub smt_state: SmtState,
    pub nonce_to_builder: HashMap<u64, (TransactionContext, ReleaseFundsBuilder)>,
}

#[derive(Clone)]
pub struct InstructionWithSigners {
    pub instructions: Vec<solana_sdk::instruction::Instruction>,
    pub fee_payer: Pubkey,
    pub signers: Vec<&'static Signer>,
    pub compute_unit_price: Option<u64>,
    pub compute_budget: Option<u32>,
}
