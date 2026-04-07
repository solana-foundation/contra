use crate::config::ProgramType;
use crate::operator::RpcClientWithRetry;
use crate::storage::common::models::TransactionStatus;
use crate::storage::common::storage::Storage;
use crate::{operator::utils::smt_util::SmtState, operator::MintCache};
use chrono::{DateTime, Utc};
use contra_escrow_program_client::instructions::{ReleaseFundsBuilder, ResetSmtRootBuilder};
use solana_keychain::Signer;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signature;
use std::collections::HashMap;
use std::sync::Arc;

use crate::operator::utils::instruction_util::{MintToBuilder, WithdrawalRemintInfo};

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
    /// Signature of the remint transaction (only set for FailedReminted status)
    pub remint_signature: Option<String>,
    /// True when a remint was attempted but failed (ManualReview). Lets consumers
    /// distinguish "remint tried and failed" from "remint never attempted".
    pub remint_attempted: bool,
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
    /// Milliseconds between `getSignatureStatuses` polls. Populated from `OperatorConfig`.
    pub confirmation_poll_interval_ms: u64,
    pub rotation_retry_queue: Vec<(TransactionContext, ReleaseFundsBuilder)>,
    /// Pending ResetSmtRoot transaction waiting for in-flight txs to settle
    pub pending_rotation: Option<Box<ResetSmtRootBuilder>>,
    pub program_type: ProgramType,
    /// Cached remint info for withdrawal transactions, keyed by nonce.
    /// Extracted before cleanup_failed_transaction removes builder from SMT cache.
    pub remint_cache: HashMap<u64, WithdrawalRemintInfo>,
    /// Signatures sent per withdrawal nonce, used for finality checks before reminting.
    pub pending_signatures: HashMap<u64, Vec<Signature>>,
    /// Deferred remint queue — entries are processed after their deadline matures.
    pub pending_remints: Vec<PendingRemint>,
}

/// A remint deferred until Solana finality window passes, allowing us to verify
/// that the original withdrawal definitively did not land before reminting.
pub struct PendingRemint {
    pub ctx: TransactionContext,
    pub remint_info: WithdrawalRemintInfo,
    pub signatures: Vec<Signature>,
    pub original_error: String,
    /// UTC timestamp after which the finality check runs. Using DateTime<Utc> instead of
    /// Instant allows the deadline to be persisted to the database and restored on restart.
    /// The minor risk of clock skew affecting a 32-second window is acceptable — the       
    /// finality check runs regardless, so a slightly early or late execution is safe.
    pub deadline: DateTime<Utc>,
    /// Number of times the finality check has been retried (e.g. due to RPC errors).
    pub finality_check_attempts: u32,
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
