use crate::channel_utils::send_guaranteed;
use crate::config::ProgramType;
use crate::error::TransactionError;
use crate::error::{AccountError, OperatorError, ProgramError};
use crate::metrics;
use crate::operator::recovery::MAX_RECOVERY_REQUEUE_ATTEMPTS;
use crate::operator::tree_constants::MAX_TREE_LEAVES;
use crate::operator::utils::instruction_util::{
    mint_extra_error_checks_policy, TransactionBuilder,
};
use crate::operator::utils::transaction_util::parse_program_error;
use crate::operator::utils::transaction_util::{
    build_and_sign, check_transaction_status, send_signed, ConfirmationResult,
    MAX_POLL_ATTEMPTS_CONFIRMATION,
};
use crate::operator::{
    sign_and_send_transaction, ExtraErrorCheckPolicy, RetryPolicy, RpcClientWithRetry,
};
use crate::storage::common::models::TransactionStatus;
use crate::storage::common::storage::{RequeueOutcome, Storage};
use chrono::Utc;
use private_channel_escrow_program_client::errors::PrivateChannelEscrowProgramError;
use private_channel_metrics::MetricLabel;
use solana_keychain::SolanaSigner;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::signature::Signature;
use tokio::sync::{mpsc, OwnedSemaphorePermit};
use tracing::{debug, error, info, info_span, warn, Instrument};

use super::mint::{cleanup_mint_builder, try_jit_mint_initialization, JitOutcome};
use super::proof::{
    cleanup_failed_transaction, pending_rotation_due, rebuild_with_regenerated_proof,
};
use super::types::{
    InFlightQueue, InFlightTx, InstructionWithSigners, PendingRemint, PendingSig, PollTaskResult,
    SendDurability, SenderState, TransactionContext, TransactionStatusUpdate, MAX_IN_FLIGHT,
};

use std::sync::Arc;

use std::time::Duration;

/// Safety delay before checking finality and reminting.
/// Solana finalized ≈ 32 slots × 400ms = ~12.8s. We use 2.5× safety factor.
pub const FINALITY_SAFETY_DELAY: Duration = Duration::from_secs(32);

const MAX_SIGS_PER_CALL: usize = 256;

impl SenderState {
    /// True if a withdrawal in `tree_index` is still parked in pending_remints
    /// awaiting finality. While true we must not build new proofs for that tree:
    /// the local SMT may disagree with chain until the nonce resolves.
    pub(super) fn has_unresolved_ambiguous_nonce(&self, tree_index: u64) -> bool {
        self.pending_remints.iter().any(|p| {
            p.ctx
                .withdrawal_nonce
                .is_some_and(|n| n / MAX_TREE_LEAVES as u64 == tree_index)
        })
    }

    /// Handle incoming transaction builder (either ReleaseFunds or Mint)
    /// For ReleaseFunds: Generate SMT proof and complete builder
    /// For Mint: Just build instruction (no proof needed)
    pub(super) async fn handle_transaction_builder(
        &mut self,
        tx_builder: TransactionBuilder,
    ) -> Result<InstructionWithSigners, OperatorError> {
        let signers = tx_builder.signers();
        let compute_unit_price = tx_builder.compute_unit_price();
        let compute_budget = tx_builder.compute_budget();

        // For now fee payer is always the first signer
        let fee_payer = match signers.first() {
            Some(s) => s.pubkey(),
            None => {
                return Err(ProgramError::InvalidBuilder {
                    reason: "No signers provided".to_string(),
                }
                .into())
            }
        };

        match tx_builder {
            TransactionBuilder::ReleaseFunds(builder_with_nonce) => {
                // Cache remint info for potential recovery on permanent failure
                if let Some(ref info) = builder_with_nonce.remint_info {
                    self.remint_cache
                        .insert(builder_with_nonce.nonce, info.clone());
                }

                // Initialize SMT state lazily if needed
                if self.smt_state.is_none() {
                    self.initialize_smt_state().await?;
                }

                self.smt_state
                    .as_mut()
                    .ok_or(ProgramError::SmtNotInitialized)?
                    .handle_release_funds_transaction(
                        builder_with_nonce,
                        fee_payer,
                        signers,
                        compute_unit_price,
                        compute_budget,
                    )
            }
            // InitializeMint transaction: creates mint account via AdminVm
            TransactionBuilder::InitializeMint(_) => Ok(InstructionWithSigners {
                instructions: tx_builder.instructions()?,
                fee_payer,
                signers,
                compute_unit_price,
                compute_budget,
            }),
            TransactionBuilder::Mint(ref builder_with_txn_id) => {
                // Cache the builder for potential JIT retry
                self.mint_builders.insert(
                    builder_with_txn_id.txn_id,
                    builder_with_txn_id.builder.clone(),
                );

                // Mint transaction: creates ATA + mints tokens
                Ok(InstructionWithSigners {
                    instructions: tx_builder.instructions()?,
                    fee_payer,
                    signers,
                    compute_unit_price,
                    compute_budget,
                })
            }
            TransactionBuilder::ResetSmtRoot(builder) => {
                // Arm first: nothing below is infallible, and a dropped builder is a
                // lost rotation. Cleared only where the sender proves the tree advanced.
                self.pending_rotation = Some(builder);

                // Initialize SMT state in case a reset is the first thing we process
                // after a restart.
                if self.smt_state.is_none() {
                    self.initialize_smt_state().await?;
                }
                let smt = self
                    .smt_state
                    .as_ref()
                    .ok_or(ProgramError::SmtNotInitialized)?;
                let in_flight_count = smt.nonce_to_builder.len();
                let expected_current_tree_index = smt.smt_state.tree_index();

                if in_flight_count > 0 {
                    info!(
                        "Rotation transaction received but {} in-flight txs exist - queuing",
                        in_flight_count
                    );

                    return Err(ProgramError::RotationPending { in_flight_count }.into());
                }

                // Bind the reset to our local tree index so the on-chain program
                // rejects a replay. Rebound on every attempt, so a retry after a
                // sync targets the current index.
                let rotation = self
                    .pending_rotation
                    .as_mut()
                    .expect("armed at the top of this arm");
                rotation.expected_current_tree_index(expected_current_tree_index);
                Ok(InstructionWithSigners {
                    instructions: vec![rotation.instruction()],
                    fee_payer,
                    signers,
                    compute_budget,
                    compute_unit_price,
                })
            }
        }
    }
}

/// Top-level handler for a single transaction submission
pub async fn handle_transaction_submission(
    state: &mut SenderState,
    tx_builder: TransactionBuilder,
    storage_tx: &mpsc::Sender<TransactionStatusUpdate>,
) {
    let ctx = TransactionContext {
        transaction_id: tx_builder.transaction_id(),
        withdrawal_nonce: tx_builder.withdrawal_nonce(),
        trace_id: tx_builder.trace_id(),
        deposit_claim_lease: None,
    };

    // For a withdrawal, which tree does its nonce belong to? None for other txs.
    let release_tree_index = match &tx_builder {
        TransactionBuilder::ReleaseFunds(builder_with_nonce) => {
            Some(builder_with_nonce.nonce / MAX_TREE_LEAVES as u64)
        }
        _ => None,
    };

    // Park the withdrawal if that tree still has an unresolved ambiguous nonce.
    // Building its proof now could use a local SMT that disagrees with chain;
    // the tick drain retries it once process_pending_remints resolves the nonce.
    if release_tree_index.is_some_and(|tree| state.has_unresolved_ambiguous_nonce(tree)) {
        // The if-let always matches here: only ReleaseFunds sets release_tree_index.
        if let TransactionBuilder::ReleaseFunds(builder_with_nonce) = tx_builder {
            debug!(
                nonce = builder_with_nonce.nonce,
                "Parking withdrawal: ambiguous nonce in same tree unresolved"
            );
            // Mark the row Parked so recovery's Processing sweep does not
            // quarantine it. Best-effort: the in-memory queue still drives it,
            // and the next heartbeat re-park repairs a write lost here.
            let id = builder_with_nonce.transaction_id;
            if let Err(e) = state.storage.try_park_processing(id).await {
                warn!(transaction_id = id, "Park status write failed: {e}");
            }
            state.ambiguous_retry_queue.push(builder_with_nonce);
        }
        // Always return: a blocked withdrawal is parked, not submitted.
        return;
    }

    let retry_policy = tx_builder.retry_policy();
    let compute_unit_price = tx_builder.compute_unit_price();
    // Owned so it can be moved into InFlightTx
    let extra_error_checks_policy = tx_builder.extra_error_checks_policy();

    let span = info_span!(
        "tx",
        trace_id = ctx.trace_id.as_deref().unwrap_or("none"),
        nonce = ctx.withdrawal_nonce.map(|n| n as i64),
    );

    async {
        match state.handle_transaction_builder(tx_builder.clone()).await {
            Ok(instruction) => {
                info!("Transaction instruction ready for submission");
                // Mint and InitializeMint use fire-and-forget: send immediately,
                // defer confirmation to the batch timer poll in `poll_in_flight`.
                // ReleaseFunds and ResetSmtRoot use the blocking path because SMT
                // proof ordering requires at-most-one in-flight withdrawal at a time.
                match &tx_builder {
                    TransactionBuilder::Mint(_) | TransactionBuilder::InitializeMint(_) => {
                        // A user-fund Mint is Recoverable (persisted write-ahead, re-minted
                        // by recovery on failure); InitializeMint mints no balance and is
                        // on-chain idempotent, so it is Terminal.
                        let durability = match &tx_builder {
                            TransactionBuilder::Mint(b) => SendDurability::Recoverable {
                                deposit_expected_updated_at: b.fetched_updated_at,
                            },
                            _ => SendDurability::Terminal,
                        };
                        spawn_fire_and_store(
                            state,
                            instruction,
                            compute_unit_price,
                            ctx.clone(),
                            retry_policy,
                            extra_error_checks_policy,
                            storage_tx.clone(),
                            durability,
                        );
                    }
                    _ => {
                        send_and_confirm(
                            state,
                            instruction,
                            compute_unit_price,
                            &ctx,
                            retry_policy,
                            &extra_error_checks_policy,
                            storage_tx,
                        )
                        .await;
                    }
                }
            }
            Err(e) => {
                route_builder_error(state, &ctx, tx_builder, storage_tx, e).await;
            }
        }
    }
    .instrument(span)
    .await;
}

/// Drive the rotation the sender owes the chain. Called on the rotation tick.
///
/// The reset has no DB row and no nonce, so `pending_rotation` is its only record.
/// It stays armed across the whole submission, which is what makes a pre-broadcast
/// or pre-confirmation failure retry here instead of dropping the rotation and
/// stranding the boundary withdrawal.
pub(super) async fn drive_pending_rotation(
    state: &mut SenderState,
    storage_tx: &mpsc::Sender<TransactionStatusUpdate>,
) {
    if !pending_rotation_due(state) {
        return;
    }
    let Some(builder) = state.pending_rotation.clone() else {
        return;
    };

    // An earlier attempt may have landed with its confirmation lost, so settle that
    // against the chain before spending another. No local SMT means no index to
    // compare against; the submission below initializes it.
    if let Some(bound) = state
        .smt_state
        .as_ref()
        .map(|smt_state| smt_state.smt_state.tree_index())
    {
        if tree_advanced_past(state, bound).await {
            info!("Tree already advanced past {bound}, rotation no longer owed");
            state.pending_rotation = None;
            return;
        }
    }

    info!("Submitting owed ResetSmtRoot");
    handle_transaction_submission(state, TransactionBuilder::ResetSmtRoot(builder), storage_tx)
        .await;

    // Still armed means this attempt did not prove the tree advanced. Kept armed so
    // the next tick retries, and logged because a reset has no row to escalate.
    if state.pending_rotation.is_some() {
        metrics::OPERATOR_TRANSACTION_ERRORS
            .with_label_values(&[state.program_type.as_label(), "rotation_not_landed"])
            .inc();
        warn!("ResetSmtRoot did not land, rotation stays armed for retry");
    }
}

/// Re-read the on-chain tree index and report whether it is past `bound`, syncing
/// the local SMT forward when it is. Short of a confirmed reset, that is the only
/// proof the rotation owed at `bound` already landed.
///
/// Only ever moves the local index forward. Callers pass their local tree index, so
/// a past-bound answer is a forward move; a lagging backend behind a load-balanced
/// endpoint can answer with an older index, and rewinding on that would clear the
/// current tree's nonces, since reset() empties the tree. A not-advanced answer and
/// a read failure both report false: the rotation stays owed, and the program's
/// expected_current_tree_index check rejects a redundant retry instead of advancing
/// the tree twice.
async fn tree_advanced_past(state: &mut SenderState, bound: u64) -> bool {
    match state.fetch_onchain_tree_index().await {
        Ok(onchain_index) if onchain_index > bound => {
            if let Some(ref mut smt_state) = state.smt_state {
                warn!("Synced local SMT forward to on-chain tree_index {onchain_index}");
                smt_state.smt_state.reset(onchain_index);
            }
            true
        }
        Ok(onchain_index) => {
            debug!("On-chain tree_index {onchain_index} not past {bound}, rotation still owed");
            false
        }
        Err(e) => {
            error!("Tree index re-fetch failed: {e}, local SMT left unchanged");
            false
        }
    }
}

/// Route a `handle_transaction_builder` error to its non-success path; separate from `handle_transaction_submission` so it is testable without real signers.
pub(super) async fn route_builder_error(
    state: &mut SenderState,
    ctx: &TransactionContext,
    tx_builder: TransactionBuilder,
    storage_tx: &mpsc::Sender<TransactionStatusUpdate>,
    err: OperatorError,
) {
    match err {
        OperatorError::Program(ProgramError::RotationPending { in_flight_count }) => {
            info!(
                "Rotation pending, waiting for {} in-flight txs to settle",
                in_flight_count
            );
        }
        OperatorError::Program(ProgramError::TreeIndexMismatch {
            nonce,
            expected_tree_index,
            current_tree_index,
        }) => {
            if let TransactionBuilder::ReleaseFunds(builder_with_nonce) = tx_builder {
                info!(
                    "Tree index mismatch: nonce {} expects {} but current is {} - queuing for retry",
                    nonce, expected_tree_index, current_tree_index
                );
                state.rotation_retry_queue.push((
                    TransactionContext {
                        transaction_id: Some(builder_with_nonce.transaction_id),
                        withdrawal_nonce: Some(builder_with_nonce.nonce),
                        trace_id: Some(builder_with_nonce.trace_id),
                        deposit_claim_lease: None,
                    },
                    builder_with_nonce.builder,
                ));
            } else {
                error!("TreeIndexMismatch for non-ReleaseFunds transaction");
            }
        }
        // Transient lazy-init read failure: RPC instance fetch (AccountNotFound)
        // or DB nonce read (Storage) from validate_smt_root. The `smt_state.is_none()`
        // guard bounds this to the pre-init window: the nonce is inserted into the SMT
        // (proof.rs) and signing both happen only after init sets smt_state to Some, and
        // it never reverts. So None proves nothing was mutated or broadcast and the
        // requeue is safe. A Storage/Account error once initialized falls through to
        // fail-closed. Requeue Pending for a bounded retry instead of freezing it for
        // recovery to quarantine.
        e @ OperatorError::Account(AccountError::AccountNotFound { .. })
        | e @ OperatorError::Storage(_)
            if state.smt_state.is_none() =>
        {
            metrics::OPERATOR_TRANSACTION_ERRORS
                .with_label_values(&[state.program_type.as_label(), "smt_init_transient_error"])
                .inc();
            match (ctx.withdrawal_nonce, ctx.transaction_id) {
                // requeue_or_fail_prebroadcast requires no stashed signature for the
                // nonce; confirm it here as send_and_confirm does. The smt_state guard
                // above already implies it, but check explicitly so the contract does
                // not rely on that reasoning holding.
                (Some(nonce), Some(transaction_id))
                    if state
                        .pending_signatures
                        .get(&nonce)
                        .is_none_or(|sigs| sigs.is_empty()) =>
                {
                    warn!(
                        transaction_id,
                        nonce, "Transient SMT init failure; requeueing withdrawal to Pending: {e}"
                    );
                    let reason = format!(
                        "SMT init failed after {MAX_RECOVERY_REQUEUE_ATTEMPTS} requeues: {e}"
                    );
                    requeue_or_fail_prebroadcast(
                        state,
                        ctx,
                        storage_tx,
                        nonce,
                        transaction_id,
                        &reason,
                    )
                    .await;
                }
                _ => {
                    // No nonce/transaction_id, or a stashed signature exists: cannot
                    // safely requeue, so leave Processing for recovery.
                    error!("Transient SMT init failure not safe to requeue; leaving row Processing: {e}");
                }
            }
        }
        // Fail closed: root mismatch (DB behind chain, a release may have landed),
        // uninitialized tree, malformed instance, missing instance, or any Account/
        // Storage error that reached here after init (smt_state.is_some(), so the
        // transient arm above did not match). Auto-replay could mask a needed DB
        // resync, so leave Processing for recovery.
        e @ OperatorError::Program(ProgramError::SmtRootMismatch { .. })
        | e @ OperatorError::Program(ProgramError::SmtNotInitialized)
        | e @ OperatorError::Account(_)
        | e @ OperatorError::Storage(_) => {
            metrics::OPERATOR_TRANSACTION_ERRORS
                .with_label_values(&[state.program_type.as_label(), "smt_init_error"])
                .inc();
            error!(
                transaction_id = ctx.transaction_id,
                nonce = ctx.withdrawal_nonce.map(|n| n as i64),
                "SMT init failed (fail-closed); leaving row Processing for recovery: {}",
                e
            );
        }
        e => {
            metrics::OPERATOR_TRANSACTION_ERRORS
                .with_label_values(&[state.program_type.as_label(), "build_error"])
                .inc();
            error!("Failed to build transaction: {}", e);
            send_fatal_error(storage_tx, ctx, &e.to_string()).await;
        }
    }
}

/// Persist a broadcast signature write-ahead (DB only), fail-closed: `Err(())` means
/// "do not broadcast". On persist failure we count the error, log it with ids, and
/// return early so the caller aborts before sending; the row stays Processing for the
/// recovery worker to reconcile against the chain.
pub(super) async fn persist_signature_or_abort(
    storage: &Storage,
    pt: &str,
    transaction_id: i64,
    signature: &Signature,
    last_valid_block_height: u64,
) -> Result<(), ()> {
    if let Err(e) = storage
        .insert_release_signature(
            transaction_id,
            signature.to_string(),
            last_valid_block_height as i64,
        )
        .await
    {
        metrics::OPERATOR_TRANSACTION_ERRORS
            .with_label_values(&[pt, "pre_send_persist_error"])
            .inc();
        let abort = TransactionError::PreSendPersistFailed {
            reason: e.to_string(),
        };
        error!(
            transaction_id,
            signature = %signature,
            "Aborting before broadcast, leaving row Processing for recovery: {}",
            abort
        );
        return Err(());
    }
    Ok(())
}

/// Sign, send, confirm, and handle the result
pub(super) async fn send_and_confirm(
    state: &mut SenderState,
    instruction: InstructionWithSigners,
    compute_unit_price: Option<u64>,
    ctx: &TransactionContext,
    retry_policy: RetryPolicy,
    extra_error_checks_policy: &ExtraErrorCheckPolicy,
    storage_tx: &mpsc::Sender<TransactionStatusUpdate>,
) {
    // Check retry limit - only for idempotent operations that can be retried at sender level
    if let Some(nonce) = ctx.withdrawal_nonce {
        match retry_policy {
            RetryPolicy::Idempotent => {
                let attempts = state.retry_counts.get(&nonce).copied().unwrap_or(0);
                if attempts >= state.retry_max_attempts {
                    metrics::OPERATOR_TRANSACTION_ERRORS
                        .with_label_values(&[state.program_type.as_label(), "max_retries_exceeded"])
                        .inc();
                    error!(
                        "Max retries ({}) exceeded for withdrawal_nonce {}",
                        state.retry_max_attempts, nonce
                    );
                    handle_permanent_failure(state, ctx, storage_tx, "Max retries exceeded").await;
                    return;
                }
                state.retry_counts.insert(nonce, attempts + 1);
                info!(
                    "Transaction attempt {}/{} for withdrawal_nonce {}",
                    attempts + 1,
                    state.retry_max_attempts,
                    nonce
                );
            }
            RetryPolicy::None => {
                info!("Sending non-idempotent transaction - single sender-level attempt");
            }
        }
    }

    let pt = state.program_type.as_label();
    let send_start = std::time::Instant::now();

    // Build and sign before broadcasting so the signature can be persisted write-ahead.
    let (transaction, signature, last_valid_block_height) =
        match build_and_sign(&state.rpc_client, instruction.clone()).await {
            Ok(signed) => signed,
            Err(e) => {
                metrics::OPERATOR_RPC_SEND_DURATION
                    .with_label_values(&[pt, "error"])
                    .observe(send_start.elapsed().as_secs_f64());
                metrics::OPERATOR_TRANSACTION_ERRORS
                    .with_label_values(&[pt, "build_sign_error"])
                    .inc();
                error!("Failed to build/sign transaction: {}", e);
                // build_and_sign failed before a signature existed. A withdrawal
                // with no signature stashed from an earlier attempt provably never
                // broadcast, so requeue it for an automatic retry; the attempt cap
                // above bounds the loop. With stashed signatures (Retry recursion
                // after a broadcast) or no nonce (ResetSmtRoot), keep the
                // permanent-failure path.
                match (ctx.withdrawal_nonce, ctx.transaction_id) {
                    (Some(nonce), Some(transaction_id))
                        if state
                            .pending_signatures
                            .get(&nonce)
                            .is_none_or(|sigs| sigs.is_empty()) =>
                    {
                        let reason = format!(
                            "build/sign failed after {MAX_RECOVERY_REQUEUE_ATTEMPTS} requeues: {e}"
                        );
                        requeue_or_fail_prebroadcast(
                            state,
                            ctx,
                            storage_tx,
                            nonce,
                            transaction_id,
                            &reason,
                        )
                        .await;
                    }
                    _ => handle_permanent_failure(state, ctx, storage_tx, &e.to_string()).await,
                }
                return;
            }
        };

    // A withdrawal nonce is consumed on broadcast, so a release that lands must already
    // have a durable signature record for crash recovery to reconcile against.
    if let (Some(nonce), Some(txid)) = (ctx.withdrawal_nonce, ctx.transaction_id) {
        if persist_signature_or_abort(
            &state.storage,
            pt,
            txid,
            &signature,
            last_valid_block_height,
        )
        .await
        .is_err()
        {
            // The persist WRITE just failed, so do not attempt another DB write here.
            // With no stashed signature this nonce provably never broadcast, so the
            // Stage-1 SMT/builder/retry/remint mutations describe a nonce the chain
            // never accepted; roll them back so later withdrawals in this tree build
            // on a root the chain agrees with. A stashed signature (retry recursion
            // after a broadcast) means a real tx may land, so leave state intact for
            // recovery. Row stays Processing either way.
            if state
                .pending_signatures
                .get(&nonce)
                .is_none_or(|sigs| sigs.is_empty())
            {
                cleanup_failed_transaction(state, Some(nonce));
            }
            return;
        }
    }

    match send_signed(&state.rpc_client, &transaction, retry_policy).await {
        // send_signed returns the same signature we already persisted; keep using it.
        Ok(_) => {
            info!("Transaction sent with signature: {}", signature);

            // Stash the in-flight signature only after a successful broadcast. A send
            // that never reached the network (e.g. a failed simulation) thus leaves no
            // stashed signature, so a permanent failure routes to ManualReview rather
            // than a deferred remint, preserving the pre-existing failure semantics.
            if let Some(nonce) = ctx.withdrawal_nonce {
                state
                    .pending_signatures
                    .entry(nonce)
                    .or_default()
                    .push(PendingSig {
                        signature,
                        last_valid_block_height,
                    });
            }

            let commitment_config = CommitmentConfig::confirmed();

            let result = check_transaction_status(
                state.rpc_client.clone(),
                &signature,
                commitment_config,
                extra_error_checks_policy,
                state.confirmation_poll_interval_ms,
            )
            .await;

            let result_label = match &result {
                Ok(ConfirmationResult::Confirmed) => "success",
                _ => "failure",
            };
            metrics::OPERATOR_RPC_SEND_DURATION
                .with_label_values(&[pt, result_label])
                .observe(send_start.elapsed().as_secs_f64());

            handle_confirmation_result(
                state,
                result,
                signature,
                compute_unit_price,
                ctx,
                instruction,
                retry_policy,
                extra_error_checks_policy,
                storage_tx,
            )
            .await;
        }
        Err(e) => {
            metrics::OPERATOR_RPC_SEND_DURATION
                .with_label_values(&[pt, "error"])
                .observe(send_start.elapsed().as_secs_f64());
            metrics::OPERATOR_TRANSACTION_ERRORS
                .with_label_values(&[pt, "rpc_send_error"])
                .inc();
            error!("Failed to send transaction: {}", e);
            handle_permanent_failure(state, ctx, storage_tx, &e.to_string()).await;
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_confirmation_result<'a>(
    state: &'a mut SenderState,
    result: Result<ConfirmationResult, crate::error::TransactionError>,
    signature: Signature,
    compute_unit_price: Option<u64>,
    ctx: &'a TransactionContext,
    instruction: InstructionWithSigners,
    retry_policy: RetryPolicy,
    extra_error_checks_policy: &'a ExtraErrorCheckPolicy,
    storage_tx: &'a mpsc::Sender<TransactionStatusUpdate>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        let pt = state.program_type.as_label();
        match result {
            Ok(ConfirmationResult::Confirmed) => {
                handle_success(state, ctx, signature, storage_tx).await;
            }
            Ok(ConfirmationResult::Failed(Some(
                PrivateChannelEscrowProgramError::InvalidSmtProof,
            ))) => {
                metrics::OPERATOR_TRANSACTION_ERRORS
                    .with_label_values(&[pt, "invalid_smt_proof"])
                    .inc();
                warn!("InvalidSmtProof - removing nonce and rebuilding with fresh proof");
                if let (Some(nonce), Some(ref mut smt_state)) =
                    (ctx.withdrawal_nonce, state.smt_state.as_mut())
                {
                    smt_state.smt_state.remove_nonce(nonce);
                }
                if let Some(new_instruction) =
                    rebuild_with_regenerated_proof(state, ctx.withdrawal_nonce, instruction).await
                {
                    send_and_confirm(
                        state,
                        new_instruction,
                        compute_unit_price,
                        ctx,
                        retry_policy,
                        extra_error_checks_policy,
                        storage_tx,
                    )
                    .await;
                } else {
                    handle_permanent_failure(state, ctx, storage_tx, "Failed to rebuild proof")
                        .await;
                }
            }
            Ok(ConfirmationResult::Failed(Some(
                PrivateChannelEscrowProgramError::InvalidTransactionNonceForCurrentTreeIndex,
            ))) => {
                metrics::OPERATOR_TRANSACTION_ERRORS
                    .with_label_values(&[pt, "invalid_nonce_for_tree_index"])
                    .inc();
                error!("InvalidTransactionNonce - fatal error");
                handle_permanent_failure(state, ctx, storage_tx, "Invalid nonce for tree index")
                    .await;
            }
            Ok(ConfirmationResult::MintNotInitialized) => {
                metrics::OPERATOR_TRANSACTION_ERRORS
                    .with_label_values(&[pt, "mint_not_initialized"])
                    .inc();
                let Some(txn_id) = ctx.transaction_id else {
                    error!("MintNotInitialized error without transaction_id");
                    handle_permanent_failure(state, ctx, storage_tx, "Mint initialization failed")
                        .await;
                    return;
                };
                if !state.mint_builders.contains_key(&txn_id) {
                    error!("MintNotInitialized error for non-Mint transaction");
                    handle_permanent_failure(state, ctx, storage_tx, "Unexpected mint error").await;
                    return;
                }
                warn!(
                    "Mint not initialized — running JIT verdict for txn {}",
                    txn_id
                );
                match try_jit_mint_initialization(state, txn_id, instruction.clone()).await {
                    JitOutcome::Retry(new_instruction) => {
                        let Some(lease) = ctx.deposit_claim_lease else {
                            metrics::OPERATOR_TRANSACTION_ERRORS
                                .with_label_values(&[pt, "jit_missing_claim_lease"])
                                .inc();
                            warn!(
                                transaction_id = txn_id,
                                "JIT retry missing deposit claim lease; leaving row Processing for recovery",
                            );
                            return;
                        };
                        // Journal the retry signature through the ownership claim before broadcast.
                        // Awaited inline since this rare retry is already off the hot path.
                        match Arc::clone(&state.semaphore).try_acquire_owned() {
                            Ok(permit) => {
                                info!(
                                    "JIT verdict: Retry - re-issuing mint via write-ahead fire-and-store for txn {}",
                                    txn_id
                                );
                                fire_and_store_task(
                                    state.rpc_client.clone(),
                                    state.storage.clone(),
                                    state.in_flight.clone(),
                                    state.program_type,
                                    new_instruction,
                                    compute_unit_price,
                                    ctx.clone(),
                                    retry_policy,
                                    mint_extra_error_checks_policy(),
                                    storage_tx.clone(),
                                    SendDurability::Recoverable {
                                        deposit_expected_updated_at: lease,
                                    },
                                    permit,
                                )
                                .await;
                            }
                            Err(_) => {
                                metrics::OPERATOR_TRANSACTION_ERRORS
                                    .with_label_values(&[pt, "in_flight_cap_exceeded"])
                                    .inc();
                                warn!(
                                    "In-flight cap reached - deferring JIT retry for txn {}; \
                                     row left Processing for recovery",
                                    txn_id
                                );
                            }
                        }
                    }
                    JitOutcome::ManualReview(reason) => {
                        metrics::OPERATOR_TRANSACTION_ERRORS
                            .with_label_values(&[pt, "mint_jit_manual_review"])
                            .inc();
                        error!("JIT verdict: ManualReview — {}", reason);
                        send_guaranteed(
                            storage_tx,
                            TransactionStatusUpdate {
                                transaction_id: txn_id,
                                trace_id: ctx.trace_id.clone(),
                                status: TransactionStatus::ManualReview,
                                counterpart_signature: None,
                                processed_at: Some(Utc::now()),
                                error_message: Some(reason),
                                remint_signature: None,
                                remint_attempted: false,
                                release_signatures: None,
                            },
                            "transaction status update",
                        )
                        .await
                        .ok();
                        // Release the cached MintToBuilder so it doesn't
                        // linger past the terminal transition. For deposits
                        // ctx.withdrawal_nonce is None, so the remint /
                        // pending_signatures cleanup is a no-op; Mirrors
                        // the cleanup pattern in handle_permanent_failure.
                        cleanup_failed_transaction(state, ctx.withdrawal_nonce);
                        state.mint_builders.remove(&txn_id);
                    }
                    JitOutcome::PermanentFailure(reason) => {
                        handle_permanent_failure(state, ctx, storage_tx, &reason).await;
                    }
                }
            }
            Ok(ConfirmationResult::Retry) => match retry_policy {
                RetryPolicy::None => {
                    metrics::OPERATOR_TRANSACTION_ERRORS
                        .with_label_values(&[pt, "confirmation_timeout_non_idempotent"])
                        .inc();
                    error!("Confirmation failed for non-idempotent operation - status unknown, cannot retry");
                    handle_permanent_failure(
                        state,
                        ctx,
                        storage_tx,
                        "Confirmation failed - transaction status unknown, unsafe to retry",
                    )
                    .await;
                }
                RetryPolicy::Idempotent => {
                    metrics::OPERATOR_TRANSACTION_ERRORS
                        .with_label_values(&[pt, "confirmation_timeout"])
                        .inc();
                    // A reset carries neither id nor nonce, so send_and_confirm's
                    // per-nonce attempt cap does not apply and re-sending here would loop
                    // unpaced until it lands. The armed rotation is retried on the
                    // rotation tick instead, which re-reads the on-chain tree index
                    // before each attempt. Both fields must be None: a deposit also has
                    // no nonce, and its retry does belong on this path.
                    if ctx.transaction_id.is_none() && ctx.withdrawal_nonce.is_none() {
                        warn!("Confirmation timed out for reset, leaving it to the rotation tick");
                        return;
                    }
                    warn!("Confirmation failed for idempotent operation - retrying (nonce protects against duplicates)");
                    send_and_confirm(
                        state,
                        instruction,
                        compute_unit_price,
                        ctx,
                        retry_policy,
                        extra_error_checks_policy,
                        storage_tx,
                    )
                    .await;
                }
            },
            Ok(ConfirmationResult::Failed(Some(
                PrivateChannelEscrowProgramError::UnexpectedTreeIndex,
            ))) => {
                metrics::OPERATOR_TRANSACTION_ERRORS
                    .with_label_values(&[pt, "reset_tree_already_advanced"])
                    .inc();
                // Rejected because the on-chain index is not the one this attempt
                // bound, normally because a reset already landed. Only an index past
                // the bound one proves that, and it is the only case the helper syncs;
                // anything else (including a lagging read) leaves the local SMT alone
                // and keeps the rotation armed to rebind on the next attempt.
                // smt_state is always Some here: the reset submit path initializes
                // it before sending, and nothing clears it back to None.
                if let Some(bound) = state
                    .smt_state
                    .as_ref()
                    .map(|smt_state| smt_state.smt_state.tree_index())
                {
                    if tree_advanced_past(state, bound).await {
                        state.pending_rotation = None;
                    }
                }
            }
            Ok(ConfirmationResult::Failed(program_error)) => {
                metrics::OPERATOR_TRANSACTION_ERRORS
                    .with_label_values(&[pt, "program_error"])
                    .inc();
                error!("Other program error: {:?}", program_error);
                handle_permanent_failure(state, ctx, storage_tx, &format!("{:?}", program_error))
                    .await;
            }
            Err(e) => {
                metrics::OPERATOR_TRANSACTION_ERRORS
                    .with_label_values(&[pt, "confirmation_error"])
                    .inc();
                error!("Confirmation error: {}", e);
                handle_permanent_failure(state, ctx, storage_tx, &e.to_string()).await;
            }
        }
    })
}

/// Handle successful transaction confirmation
pub(super) async fn handle_success(
    state: &mut SenderState,
    ctx: &TransactionContext,
    signature: Signature,
    storage_tx: &mpsc::Sender<TransactionStatusUpdate>,
) {
    info!("Transaction confirmed: {}", signature);

    // Handle ReleaseFunds (withdrawal nonce-based) transactions
    if let (Some(nonce), Some(ref mut smt_state)) = (ctx.withdrawal_nonce, state.smt_state.as_mut())
    {
        smt_state.nonce_to_builder.remove(&nonce);
        state.retry_counts.remove(&nonce);
        state.remint_cache.remove(&nonce);
        state.pending_signatures.remove(&nonce);
        info!("Cleaned up state for withdrawal_nonce {}", nonce);

        metrics::OPERATOR_MINTS_SENT
            .with_label_values(&[state.program_type.as_label()])
            .inc();

        if let Some(txn_id) = ctx.transaction_id {
            send_guaranteed(
                storage_tx,
                TransactionStatusUpdate {
                    transaction_id: txn_id,
                    trace_id: ctx.trace_id.clone(),
                    status: TransactionStatus::Completed,
                    counterpart_signature: Some(signature.to_string()),
                    processed_at: Some(Utc::now()),
                    error_message: None,
                    remint_signature: None,
                    remint_attempted: false,
                    release_signatures: None,
                },
                "transaction status update",
            )
            .await
            .ok();
        }
    }
    // Handle Mint (transaction_id-based) transactions
    else if let Some(transaction_id) = ctx.transaction_id {
        info!("Updating database for transaction_id {}", transaction_id);

        metrics::OPERATOR_MINTS_SENT
            .with_label_values(&[state.program_type.as_label()])
            .inc();

        cleanup_mint_builder(state, Some(transaction_id));

        send_guaranteed(
            storage_tx,
            TransactionStatusUpdate {
                transaction_id,
                trace_id: ctx.trace_id.clone(),
                status: TransactionStatus::Completed,
                counterpart_signature: Some(signature.to_string()),
                processed_at: Some(Utc::now()),
                error_message: None,
                remint_signature: None,
                remint_attempted: false,
                release_signatures: None,
            },
            "transaction status update",
        )
        .await
        .ok();
    }
    // Handle ResetSmtRoot (no transaction_id) - the owed rotation landed, so advance
    // the local SMT and disarm it. One of the two places a rotation is cleared.
    else if let Some(ref mut smt_state) = state.smt_state {
        let new_tree_index = smt_state.smt_state.tree_index() + 1;
        smt_state.smt_state.reset(new_tree_index);
        state.pending_rotation = None;
        info!(
            "Tree rotation complete! Updated local SMT to tree_index {}",
            new_tree_index
        );
    }
}

/// Leave a persisted transaction Processing after an uncertain terminal outcome.
/// The broadcast may have landed, so a terminal Failed would strand a possibly-funded
/// deposit and drop the signature recovery needs; recovery reconciles it next sweep.
fn leave_processing_for_recovery(
    pt: &str,
    transaction_id: Option<i64>,
    signature: &Signature,
    reason: &str,
) {
    metrics::OPERATOR_TRANSACTION_ERRORS
        .with_label_values(&[pt, "left_processing_for_recovery"])
        .inc();
    warn!(
        transaction_id,
        signature = %signature,
        "{reason}; leaving row Processing for recovery to reconcile",
    );
}

/// Bounded pre-broadcast requeue for a withdrawal with no stashed signature (caller
/// confirms that). Rolls back the nonce's local SMT, then does one cap-gated write:
/// requeue Processing → Pending under the cap, else escalate to ManualReview. The cap
/// lives in the write, so no separate counter read can fail and loop the row forever.
/// Keeps `retry_counts` so `send_and_confirm`'s attempt cap still bounds the loop.
pub(super) async fn requeue_or_fail_prebroadcast(
    state: &mut SenderState,
    ctx: &TransactionContext,
    storage_tx: &mpsc::Sender<TransactionStatusUpdate>,
    nonce: u64,
    transaction_id: i64,
    reason_at_cap: &str,
) {
    let pt = state.program_type.as_label();

    // Nothing broadcast; the next attempt re-inserts the nonce. Runs regardless of outcome.
    if let Some(ref mut smt_state) = state.smt_state {
        if smt_state.smt_state.remove_nonce(nonce) {
            warn!("Rolled back SMT state for nonce {nonce} after pre-broadcast failure");
        } else {
            // Builder inserted this nonce before build/sign ran, so a miss means
            // the local SMT disagrees with the row being requeued.
            error!("Nonce {nonce} missing from local SMT during pre-broadcast rollback");
        }
        smt_state.nonce_to_builder.remove(&nonce);
    }

    match state
        .storage
        .try_requeue_prebroadcast(transaction_id, MAX_RECOVERY_REQUEUE_ATTEMPTS)
        .await
    {
        Ok(RequeueOutcome::Requeued { attempts }) => {
            // Re-inserted by handle_transaction_builder on the next attempt.
            state.remint_cache.remove(&nonce);
            metrics::OPERATOR_TRANSACTION_ERRORS
                .with_label_values(&[pt, "prebroadcast_requeued"])
                .inc();
            info!(
                transaction_id,
                nonce, attempts, "Requeued withdrawal to Pending after pre-broadcast failure"
            );
        }
        Ok(RequeueOutcome::AtCap) => {
            // Keep remint_cache: handle_permanent_failure consumes it to route a
            // no-signature withdrawal to ManualReview, not a bare Failed.
            metrics::OPERATOR_TRANSACTION_ERRORS
                .with_label_values(&[pt, "prebroadcast_requeue_cap"])
                .inc();
            handle_permanent_failure(state, ctx, storage_tx, reason_at_cap).await;
        }
        Ok(RequeueOutcome::NotProcessing) => {
            state.remint_cache.remove(&nonce);
            warn!(
                transaction_id,
                nonce, "Pre-broadcast requeue skipped: row no longer Processing"
            );
        }
        Err(e) => {
            // Write failed, nothing requeued: row stays Processing for recovery. No
            // loop, since a loop needs a successful requeue.
            state.remint_cache.remove(&nonce);
            warn!(
                transaction_id,
                nonce, "Pre-broadcast requeue write failed, row left Processing for recovery: {e}"
            );
        }
    }
}

/// Handle permanent transaction failure with deferred remint for withdrawals.
///
/// For withdrawal transactions: removes remint info from cache, runs cleanup
/// (which removes the nonce from SMT and builder caches), then queues a deferred
/// remint that will execute after the Solana finality window passes. This prevents
/// double-spend if the original withdrawal lands on-chain after our polling window.
///
/// For non-withdrawal transactions: delegates to send_fatal_error.
pub(super) async fn handle_permanent_failure(
    state: &mut SenderState,
    ctx: &TransactionContext,
    storage_tx: &mpsc::Sender<TransactionStatusUpdate>,
    error_msg: &str,
) {
    // Extract remint info BEFORE cleanup destroys builder cache
    let remint_info = ctx
        .withdrawal_nonce
        .and_then(|nonce| state.remint_cache.remove(&nonce));

    // Collect stashed signatures for finality check
    let signatures = ctx
        .withdrawal_nonce
        .and_then(|nonce| state.pending_signatures.remove(&nonce))
        .unwrap_or_default();

    cleanup_failed_transaction(state, ctx.withdrawal_nonce);

    let Some(info) = remint_info else {
        // Not a withdrawal — use normal fatal error path
        send_fatal_error(storage_tx, ctx, error_msg).await;
        return;
    };

    // Zero signatures means no broadcast succeeded for this nonce, but the RPC
    // may still have broadcast one before erroring, so blind remint is unsafe.
    if signatures.is_empty() {
        error!(
            "No signatures to verify for nonce {:?} — cannot safely remint, sending to ManualReview",
            ctx.withdrawal_nonce,
        );
        if let Some(transaction_id) = ctx.transaction_id {
            send_guaranteed(
                storage_tx,
                TransactionStatusUpdate {
                    transaction_id,
                    trace_id: ctx.trace_id.clone(),
                    status: TransactionStatus::ManualReview,
                    counterpart_signature: None,
                    processed_at: Some(Utc::now()),
                    error_message: Some(format!(
                        "{} | no signatures to verify — remint unsafe",
                        error_msg
                    )),
                    remint_signature: None,
                    remint_attempted: false,
                    release_signatures: None,
                },
                "transaction status update",
            )
            .await
            .ok();
        }
        return;
    }

    let deadline = Utc::now() + chrono::Duration::from_std(FINALITY_SAFETY_DELAY).unwrap();

    // Atomically transition to PendingRemint, persisting the withdrawal signatures
    // needed for the finality check. This replaces the previous Failed write —
    // keeping status as Processing until the remint resolves avoids partial state
    // if the operator crashes during the finality window.
    if let Some(transaction_id) = ctx.transaction_id {
        let sig_strings: Vec<String> = signatures
            .iter()
            .map(|pending_sig| pending_sig.signature.to_string())
            .collect();
        let lvbhs: Vec<i64> = signatures
            .iter()
            .map(|pending_sig| pending_sig.last_valid_block_height as i64)
            .collect();

        if let Err(e) = state
            .storage
            .set_pending_remint(transaction_id, sig_strings, lvbhs, deadline)
            .await
        {
            error!(
                "Failed to persist PendingRemint for transaction {} - sending to manual review: {}",
                transaction_id, e
            );
            send_guaranteed(
                storage_tx,
                TransactionStatusUpdate {
                    transaction_id,
                    trace_id: ctx.trace_id.clone(),
                    status: TransactionStatus::ManualReview,
                    counterpart_signature: None,
                    processed_at: Some(Utc::now()),
                    error_message: Some(format!(
                        "{} | failed to persist pending remint: {}",
                        error_msg, e
                    )),
                    remint_signature: None,
                    remint_attempted: false,
                    release_signatures: None,
                },
                "transaction status update",
            )
            .await
            .ok();
            return;
        }
    }

    // `transaction_id` is always `Some` at this point in practice — only
    // `ReleaseFunds` transactions populate `remint_cache`, and `ReleaseFunds`
    // always carries a DB transaction_id (see `TransactionBuilder::transaction_id`
    // in instruction_util.rs). `InitializeMint` and `ResetSmtRoot` return `None`
    // there and would have exited early above via `send_fatal_error`. This guard
    // exists to prevent silently enqueuing a `PendingRemint` with no DB record,
    // which would be lost on restart since recovery reads from the DB.
    if ctx.transaction_id.is_none() {
        error!(
            "Cannot defer remint for nonce {:?} — no transaction_id, entry would be unrecoverable on restart",
            ctx.withdrawal_nonce,
        );
        return;
    }

    info!(
        "Remint deferred for finality check ({}s) — {} signature(s) to verify for nonce {:?}",
        FINALITY_SAFETY_DELAY.as_secs(),
        signatures.len(),
        ctx.withdrawal_nonce,
    );

    state.pending_remints.push(PendingRemint {
        ctx: ctx.clone(),
        remint_info: info,
        signatures,
        original_error: error_msg.to_string(),
        deadline,
        finality_check_attempts: 0,
    });
}

/// Sign, send, and store a Mint or InitializeMint tx in `state.in_flight`.
///
/// Called from the `route_poll_results` retry path where the caller already holds a
/// semaphore permit (carried inside the timed-out `InFlightTx`).  The permit transfers
/// to the new `InFlightTx` on success, or is dropped (slot released) on send failure.
///
/// New incoming transactions use `spawn_fire_and_store` instead, which acquires the
/// permit and offloads the blocking send to a background task.
#[allow(clippy::too_many_arguments)]
pub(super) async fn fire_and_store(
    state: &mut SenderState,
    instruction: InstructionWithSigners,
    compute_unit_price: Option<u64>,
    ctx: TransactionContext,
    retry_policy: RetryPolicy,
    extra_error_checks_policy: ExtraErrorCheckPolicy,
    storage_tx: &mpsc::Sender<TransactionStatusUpdate>,
    resend_count: u32,
    permit: OwnedSemaphorePermit,
) {
    let pt = state.program_type.as_label();
    let send_start = std::time::Instant::now();

    match sign_and_send_transaction(state.rpc_client.clone(), instruction.clone(), retry_policy)
        .await
    {
        Ok((signature, _last_valid_block_height)) => {
            metrics::OPERATOR_RPC_SEND_DURATION
                .with_label_values(&[pt, "in_flight"])
                .observe(send_start.elapsed().as_secs_f64());
            info!("Transaction sent: {}", signature);
            // push() also notifies the poll task if it is waiting on an empty queue.
            // Only InitializeMint is resent here, and it mints no balance, so no persist.
            state.in_flight.push(InFlightTx {
                signature,
                ctx,
                instruction,
                compute_unit_price,
                retry_policy,
                extra_error_checks_policy,
                poll_attempts: 0,
                resend_count,
                persisted: false,
                permit,
            });
        }
        Err(e) => {
            drop(permit);
            metrics::OPERATOR_RPC_SEND_DURATION
                .with_label_values(&[pt, "error"])
                .observe(send_start.elapsed().as_secs_f64());
            metrics::OPERATOR_TRANSACTION_ERRORS
                .with_label_values(&[pt, "rpc_send_error"])
                .inc();
            error!("Failed to send transaction (fire-and-forget): {}", e);
            handle_permanent_failure(state, &ctx, storage_tx, &e.to_string()).await;
        }
    }
}

/// Acquire a semaphore permit and spawn a background task that signs and sends
/// the transaction without blocking the sender loop's `recv` arm.
///
/// The permit is held from acquisition until the entry reaches a terminal state:
///  - **Success**: permit moves into `InFlightTx` in `in_flight`; dropped when the
///    poll task (or drain loop) confirms the tx.
///  - **Send error**: permit dropped before reporting the failure to storage.
///
/// Returns `false` if the semaphore is already at `MAX_IN_FLIGHT` capacity.  The DB
/// status is left unchanged so the fetcher re-emits the transaction on the next poll
/// cycle.
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_fire_and_store(
    state: &SenderState,
    instruction: InstructionWithSigners,
    compute_unit_price: Option<u64>,
    ctx: TransactionContext,
    retry_policy: RetryPolicy,
    extra_error_checks_policy: ExtraErrorCheckPolicy,
    storage_tx: mpsc::Sender<TransactionStatusUpdate>,
    durability: SendDurability,
) -> bool {
    let permit = match Arc::clone(&state.semaphore).try_acquire_owned() {
        Ok(p) => p,
        Err(_) => {
            metrics::OPERATOR_TRANSACTION_ERRORS
                .with_label_values(&[state.program_type.as_label(), "in_flight_cap_exceeded"])
                .inc();
            warn!(
                "In-flight cap ({MAX_IN_FLIGHT}) reached — skipping send for txn {:?}; \
                 DB status unchanged, will be re-fetched",
                ctx.transaction_id,
            );
            return false;
        }
    };

    let rpc_client = state.rpc_client.clone();
    let in_flight = state.in_flight.clone();
    let program_type = state.program_type;
    let storage = state.storage.clone();

    tokio::spawn(fire_and_store_task(
        rpc_client,
        storage,
        in_flight,
        program_type,
        instruction,
        compute_unit_price,
        ctx,
        retry_policy,
        extra_error_checks_policy,
        storage_tx,
        durability,
        permit,
    ));

    true
}

/// Build, sign, and persist the signature write-ahead when `durability` is `Recoverable`,
/// then broadcast and stash the in-flight tx. A persist failure aborts before broadcast
/// and leaves the row Processing for recovery. Split from `spawn_fire_and_store` so tests
/// can await it directly without `tokio::spawn`.
#[allow(clippy::too_many_arguments)]
pub(super) async fn fire_and_store_task(
    rpc_client: Arc<RpcClientWithRetry>,
    storage: Arc<Storage>,
    in_flight: Arc<InFlightQueue>,
    program_type: ProgramType,
    instruction: InstructionWithSigners,
    compute_unit_price: Option<u64>,
    ctx: TransactionContext,
    retry_policy: RetryPolicy,
    extra_error_checks_policy: ExtraErrorCheckPolicy,
    storage_tx: mpsc::Sender<TransactionStatusUpdate>,
    durability: SendDurability,
    permit: OwnedSemaphorePermit,
) {
    let mut ctx = ctx;
    let pt = program_type.as_label();
    let send_start = std::time::Instant::now();

    let (transaction, signature, last_valid_block_height) = match build_and_sign(
        &rpc_client,
        instruction.clone(),
    )
    .await
    {
        Ok(signed) => signed,
        Err(e) => {
            drop(permit);
            metrics::OPERATOR_RPC_SEND_DURATION
                .with_label_values(&[pt, "error"])
                .observe(send_start.elapsed().as_secs_f64());
            metrics::OPERATOR_TRANSACTION_ERRORS
                .with_label_values(&[pt, "build_sign_error"])
                .inc();
            // build_and_sign fetches a blockhash and calls the signer before any
            // signature exists or is broadcast. A Recoverable mint that fails here
            // minted no tokens, so a terminal Failed would strand the deposit: no
            // worker re-claims a Failed row. Leave it Processing so the recovery sweep
            // sees no signature and re-mints it. Terminal sends (InitializeMint) mint
            // no balance, so fail fast.
            match durability {
                SendDurability::Recoverable { .. } => {
                    metrics::OPERATOR_TRANSACTION_ERRORS
                        .with_label_values(&[pt, "left_processing_for_recovery"])
                        .inc();
                    warn!(
                            transaction_id = ctx.transaction_id,
                            "Build/sign failed for recoverable mint before broadcast; leaving row Processing for recovery: {}",
                            e
                        );
                }
                SendDurability::Terminal => {
                    error!("Failed to build/sign transaction (fire-and-forget): {}", e);
                    send_fatal_error(&storage_tx, &ctx, &e.to_string()).await;
                }
            }
            return;
        }
    };

    let persisted = match durability {
        SendDurability::Recoverable {
            deposit_expected_updated_at,
        } => {
            let Some(txid) = ctx.transaction_id else {
                // Persist required but no transaction_id to key on: abort before broadcasting an unrecoverable mint.
                drop(permit);
                metrics::OPERATOR_TRANSACTION_ERRORS
                    .with_label_values(&[pt, "pre_send_persist_error"])
                    .inc();
                error!("Persist required but transaction has no id; aborting before broadcast");
                return;
            };
            match storage
                .claim_and_persist_deposit_signature(
                    txid,
                    deposit_expected_updated_at,
                    signature.to_string(),
                    last_valid_block_height as i64,
                )
                .await
            {
                Ok(Some(lease)) => {
                    ctx.deposit_claim_lease = Some(lease);
                    true
                }
                Ok(None) => {
                    drop(permit);
                    metrics::OPERATOR_TRANSACTION_ERRORS
                        .with_label_values(&[pt, "deposit_ownership_lost"])
                        .inc();
                    warn!(
                        transaction_id = txid,
                        signature = %signature,
                        "Deposit ownership lost before broadcast; dropping stale builder without minting",
                    );
                    return;
                }
                Err(e) => {
                    drop(permit);
                    metrics::OPERATOR_TRANSACTION_ERRORS
                        .with_label_values(&[pt, "pre_send_persist_error"])
                        .inc();
                    error!(
                        transaction_id = txid,
                        signature = %signature,
                        "Aborting before broadcast, leaving row Processing for recovery: {e}",
                    );
                    return;
                }
            }
        }
        SendDurability::Terminal => false,
    };

    match send_signed(&rpc_client, &transaction, retry_policy).await {
        // send_signed returns the same signature we already hold; keep using it.
        Ok(_) => {
            metrics::OPERATOR_RPC_SEND_DURATION
                .with_label_values(&[pt, "in_flight"])
                .observe(send_start.elapsed().as_secs_f64());
            info!("Transaction sent: {}", signature);
            in_flight.push(InFlightTx {
                signature,
                ctx,
                instruction,
                compute_unit_price,
                retry_policy,
                extra_error_checks_policy,
                poll_attempts: 0,
                resend_count: 0,
                persisted,
                permit,
            });
        }
        Err(e) => {
            drop(permit);
            metrics::OPERATOR_RPC_SEND_DURATION
                .with_label_values(&[pt, "error"])
                .observe(send_start.elapsed().as_secs_f64());
            metrics::OPERATOR_TRANSACTION_ERRORS
                .with_label_values(&[pt, "rpc_send_error"])
                .inc();
            error!("Failed to send transaction (fire-and-forget): {}", e);
            // A persisted mint may already have landed, and even a preflight rejection can be
            // a stale-node false negative, so a terminal Failed would strand a funded deposit
            // and drop the signature recovery needs. Leave it Processing for recovery to
            // reconcile against the persisted signature. Terminal sends mint no balance, so fail fast.
            if persisted {
                leave_processing_for_recovery(
                    pt,
                    ctx.transaction_id,
                    &signature,
                    "send error after write-ahead persist",
                );
            } else {
                send_fatal_error(&storage_tx, &ctx, &e.to_string()).await;
            }
        }
    }
}

/// Route a batch of `(InFlightTx, Option<TransactionStatus>)` pairs returned by a
/// `getSignatureStatuses` call.
///
/// Called from both `poll_in_flight` (test / shutdown drain path) and the sender
/// loop's `poll_result_rx` arm (normal production path).
///
/// Unconfirmed entries are pushed back into `state.in_flight`, which automatically
/// re-arms the poll task's `Notify` for the next cycle.
pub(super) async fn route_poll_results(
    state: &mut SenderState,
    results: Vec<(
        InFlightTx,
        Option<solana_transaction_status::TransactionStatus>,
    )>,
    storage_tx: &mpsc::Sender<TransactionStatusUpdate>,
) {
    for (mut tx, status_opt) in results {
        match status_opt {
            Some(status) if status.satisfies_commitment(CommitmentConfig::finalized()) => {
                // Free this finalized tx's in-flight slot now so a continuation (the JIT
                // mint retry) can reuse it instead of being refused when in-flight is full.
                drop(tx.permit);
                let result = if let Some(err) = &status.err {
                    let mut extra_result = None;
                    if let ExtraErrorCheckPolicy::Extra(ref checks) = tx.extra_error_checks_policy {
                        for check in checks.iter() {
                            if let Some(r) = check(err) {
                                extra_result = Some(Ok(r));
                                break;
                            }
                        }
                    }
                    extra_result
                        .unwrap_or_else(|| Ok(ConfirmationResult::Failed(parse_program_error(err))))
                } else {
                    Ok(ConfirmationResult::Confirmed)
                };

                handle_confirmation_result(
                    state,
                    result,
                    tx.signature,
                    tx.compute_unit_price,
                    &tx.ctx,
                    tx.instruction,
                    tx.retry_policy,
                    &tx.extra_error_checks_policy,
                    storage_tx,
                )
                .await;
            }
            _ => {
                tx.poll_attempts += 1;
                if tx.poll_attempts >= MAX_POLL_ATTEMPTS_CONFIRMATION {
                    match tx.retry_policy {
                        RetryPolicy::None => {
                            metrics::OPERATOR_TRANSACTION_ERRORS
                                .with_label_values(&[
                                    state.program_type.as_label(),
                                    "confirmation_timeout_non_idempotent",
                                ])
                                .inc();
                            if tx.persisted {
                                leave_processing_for_recovery(
                                    state.program_type.as_label(),
                                    tx.ctx.transaction_id,
                                    &tx.signature,
                                    "confirmation timeout after write-ahead persist",
                                );
                            } else {
                                warn!(
                                    "Confirmation timeout for non-idempotent tx {} after {} polls - permanent failure",
                                    tx.signature, tx.poll_attempts,
                                );
                                handle_permanent_failure(
                                    state,
                                    &tx.ctx,
                                    storage_tx,
                                    "Confirmation failed - transaction status unknown, unsafe to retry",
                                )
                                .await;
                            }
                        }
                        RetryPolicy::Idempotent => {
                            // This resend broadcasts a fresh unpersisted signature, so a persisted
                            // mint must never reach it (Mint is RetryPolicy::None); assert it.
                            debug_assert!(
                                !tx.persisted,
                                "a write-ahead-persisted tx must not use the idempotent resend path"
                            );
                            metrics::OPERATOR_TRANSACTION_ERRORS
                                .with_label_values(&[
                                    state.program_type.as_label(),
                                    "confirmation_timeout",
                                ])
                                .inc();

                            let next_resend = tx.resend_count + 1;
                            if next_resend > state.retry_max_attempts {
                                metrics::OPERATOR_TRANSACTION_ERRORS
                                    .with_label_values(&[
                                        state.program_type.as_label(),
                                        "confirmation_timeout_resend_limit",
                                    ])
                                    .inc();
                                if tx.persisted {
                                    leave_processing_for_recovery(
                                        state.program_type.as_label(),
                                        tx.ctx.transaction_id,
                                        &tx.signature,
                                        "resend limit reached after write-ahead persist",
                                    );
                                } else {
                                    warn!(
                                        "Confirmation timeout for idempotent tx {} - resend limit ({}) reached, permanent failure",
                                        tx.signature, state.retry_max_attempts,
                                    );
                                    handle_permanent_failure(
                                        state,
                                        &tx.ctx,
                                        storage_tx,
                                        "Confirmation timeout: resend limit exceeded",
                                    )
                                    .await;
                                }
                            } else {
                                warn!(
                                    "Confirmation timeout for idempotent tx {} after {} polls — re-sending (attempt {}/{})",
                                    tx.signature, tx.poll_attempts, next_resend, state.retry_max_attempts,
                                );
                                fire_and_store(
                                    state,
                                    tx.instruction,
                                    tx.compute_unit_price,
                                    tx.ctx,
                                    tx.retry_policy,
                                    tx.extra_error_checks_policy,
                                    storage_tx,
                                    next_resend,
                                    tx.permit, // transfer permit to new InFlightTx
                                )
                                .await;
                            }
                        }
                    }
                } else {
                    // Still pending — push back into the shared queue.
                    // push() notifies the poll task so it wakes on the next cycle.
                    state.in_flight.push(tx);
                }
            }
        }
    }
}

/// Why a chunked status fetch was rejected. Both variants make the caller reinsert
/// the batch and retry; the split only picks the metric reason label.
#[derive(Debug)]
enum StatusFetchError {
    /// A chunk response length did not equal the request (short or oversized).
    MalformedLength,
    /// The RPC call itself failed after retries.
    Rpc,
}

impl StatusFetchError {
    fn reason(&self) -> &'static str {
        match self {
            StatusFetchError::MalformedLength => "malformed_status_response",
            StatusFetchError::Rpc => "status_poll_rpc_error",
        }
    }
}

/// Fetch statuses in `MAX_SIGS_PER_CALL` chunks. `getSignatureStatuses` is positional, so a
/// chunk whose length differs from the request would misalign every later status; reject it.
/// Returns `Err` on any RPC error or length mismatch so the caller reinserts the batch and retries.
async fn fetch_statuses_checked(
    rpc_client: &RpcClientWithRetry,
    signatures: &[Signature],
) -> Result<Vec<Option<solana_transaction_status::TransactionStatus>>, StatusFetchError> {
    let mut statuses = Vec::with_capacity(signatures.len());
    for chunk in signatures.chunks(MAX_SIGS_PER_CALL) {
        match rpc_client.get_signature_statuses(chunk).await {
            Ok(resp) if resp.value.len() == chunk.len() => statuses.extend(resp.value),
            Ok(resp) => {
                warn!(
                    "getSignatureStatuses returned {} statuses for {} signatures \
                     ({} in-flight) - treating as RPC failure, will retry next tick",
                    resp.value.len(),
                    chunk.len(),
                    signatures.len()
                );
                return Err(StatusFetchError::MalformedLength);
            }
            Err(e) => {
                warn!(
                    "getSignatureStatuses failed ({} in-flight) - will retry next tick: {}",
                    signatures.len(),
                    e
                );
                return Err(StatusFetchError::Rpc);
            }
        }
    }
    Ok(statuses)
}

/// Single-cycle poll: drain the shared queue, call `getSignatureStatuses`, then
/// route results via `route_poll_results`.
///
/// Used by `drain_in_flight` (shutdown) and by tests.  Normal production polling
/// is handled by the dedicated `run_poll_task` task so it doesn't block the send loop.
pub(super) async fn poll_in_flight(
    state: &mut SenderState,
    storage_tx: &mpsc::Sender<TransactionStatusUpdate>,
) {
    if state.in_flight.is_empty() {
        return;
    }
    let batch = state.in_flight.drain_all();
    let signatures: Vec<Signature> = batch.iter().map(|t| t.signature).collect();

    let statuses = match fetch_statuses_checked(&state.rpc_client, &signatures).await {
        Ok(s) => s,
        Err(e) => {
            metrics::OPERATOR_TRANSACTION_ERRORS
                .with_label_values(&[state.program_type.as_label(), e.reason()])
                .inc();
            // Reinsert the full batch so the next drain_in_flight iteration retries.
            state.in_flight.push_all(batch);
            return;
        }
    };

    let results: Vec<_> = batch.into_iter().zip(statuses).collect();
    route_poll_results(state, results, storage_tx).await;
}

/// Dedicated poll task: sleeps until entries arrive, then batches
/// `getSignatureStatuses` calls and forwards raw results to the sender loop.
///
/// Running in a separate task means `getSignatureStatuses` RPC latency (~50–200 ms)
/// never blocks the sender from processing new incoming transactions.
///
/// # No busy loop
/// The task waits on `in_flight.notify` (a `tokio::sync::Notify`) before each cycle.
/// Every `InFlightQueue::push` call fires `notify_one`, which stores at most one permit,
/// so the task wakes exactly once per "there is work" event even if many entries are
/// added simultaneously.  When the queue drains to zero and no new entries arrive the
/// task blocks indefinitely — zero CPU while idle.
/// Dedicated async task that owns the confirmation polling loop.
///
/// Confirmed-success entries are handled entirely within this task:
/// the `Completed` storage update is sent and `OPERATOR_MINTS_SENT` is
/// incremented without touching `SenderState`.  Only on-chain errors and
/// confirmation timeouts — rare events — are forwarded to the sender loop
/// via `result_tx` as `PollTaskResult::NeedsRouting`.  Unconfirmed entries
/// are pushed straight back into `in_flight`.
///
/// This means the `Some(results) = poll_result_rx.recv()` arm in the main
/// `select!` loop fires only for exceptions, keeping the common path off the
/// main task entirely.
pub(super) async fn run_poll_task(
    in_flight: Arc<InFlightQueue>,
    result_tx: mpsc::Sender<Vec<PollTaskResult>>,
    rpc_client: Arc<RpcClientWithRetry>,
    storage_tx: mpsc::Sender<TransactionStatusUpdate>,
    program_type: ProgramType,
    poll_interval_ms: u64,
    cancellation_token: tokio_util::sync::CancellationToken,
) {
    // Reused across poll cycles to avoid per-cycle heap allocation.
    // Signature is Copy ([u8; 64]) so extend() is a plain memcopy.
    let mut signatures: Vec<Signature> = Vec::with_capacity(MAX_IN_FLIGHT);

    loop {
        // Block until at least one entry is present (no busy loop when idle).
        tokio::select! {
            _ = cancellation_token.cancelled() => break,
            _ = in_flight.notify.notified() => {},
        }

        // Sleep the poll interval to batch entries that arrive in quick succession.
        tokio::select! {
            _ = cancellation_token.cancelled() => break,
            _ = tokio::time::sleep(tokio::time::Duration::from_millis(poll_interval_ms)) => {},
        }

        let batch = in_flight.drain_all();
        if batch.is_empty() {
            continue;
        }

        signatures.clear();
        signatures.extend(batch.iter().map(|t| t.signature));

        let statuses = match fetch_statuses_checked(&rpc_client, &signatures).await {
            Ok(s) => s,
            Err(e) => {
                metrics::OPERATOR_TRANSACTION_ERRORS
                    .with_label_values(&[program_type.as_label(), e.reason()])
                    .inc();
                // Put everything back in one lock acquisition and retry next tick.
                in_flight.push_all(batch);
                continue;
            }
        };

        let mut results: Vec<PollTaskResult> = Vec::with_capacity(batch.len());

        for (mut tx, status_opt) in batch.into_iter().zip(statuses) {
            match status_opt {
                Some(status) if status.satisfies_commitment(CommitmentConfig::finalized()) => {
                    if status.err.is_none() {
                        // ── Finalized success (hot path) ──────────────────────────────
                        // Handle entirely here — no need to wake the sender loop.
                        metrics::OPERATOR_MINTS_SENT
                            .with_label_values(&[program_type.as_label()])
                            .inc();

                        if let Some(txn_id) = tx.ctx.transaction_id {
                            if storage_tx
                                .send(TransactionStatusUpdate {
                                    transaction_id: txn_id,
                                    trace_id: tx.ctx.trace_id,
                                    status: TransactionStatus::Completed,
                                    counterpart_signature: Some(tx.signature.to_string()),
                                    processed_at: Some(Utc::now()),
                                    error_message: None,
                                    remint_signature: None,
                                    remint_attempted: false,
                                    release_signatures: None,
                                })
                                .await
                                .is_err()
                            {
                                warn!(
                                    "Storage channel closed — Completed update lost for txn {}",
                                    txn_id
                                );
                            }
                        }
                        // Notify sender loop to clean up mint_builders (O(1) HashMap remove).
                        results.push(PollTaskResult::ConfirmedSuccess(tx.ctx.transaction_id));
                    } else {
                        // ── Confirmed with on-chain error ─────────────────────────────
                        // Needs SenderState for error routing (cleanup, remint, etc.).
                        results.push(PollTaskResult::NeedsRouting(Box::new(tx), Some(status)));
                    }
                }
                _ => {
                    // ── Not yet confirmed ─────────────────────────────────────────────
                    // If we're one poll away from MAX, hand to the sender loop so it can
                    // run the timeout branch (which needs SenderState).  Otherwise push
                    // straight back — no result channel traffic needed.
                    if tx.poll_attempts + 1 >= MAX_POLL_ATTEMPTS_CONFIRMATION {
                        // Do NOT increment here; route_poll_results will increment it
                        // to MAX and fire the timeout branch.
                        results.push(PollTaskResult::NeedsRouting(Box::new(tx), None));
                    } else {
                        tx.poll_attempts += 1;
                        in_flight.push(tx);
                    }
                }
            }
        }

        if !results.is_empty() && result_tx.send(results).await.is_err() {
            break; // Sender loop gone — clean up and exit.
        }
    }
}

/// Helper for fatal errors (Failed status, no signature)
pub(super) async fn send_fatal_error(
    storage_tx: &mpsc::Sender<TransactionStatusUpdate>,
    ctx: &TransactionContext,
    error_msg: &str,
) {
    if let Some(transaction_id) = ctx.transaction_id {
        send_guaranteed(
            storage_tx,
            TransactionStatusUpdate {
                transaction_id,
                trace_id: ctx.trace_id.clone(),
                status: TransactionStatus::Failed,
                counterpart_signature: None,
                processed_at: Some(Utc::now()),
                error_message: Some(error_msg.to_string()),
                remint_signature: None,
                remint_attempted: false,
                release_signatures: None,
            },
            "transaction status update",
        )
        .await
        .ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProgramType;
    use crate::operator::sender::types::SenderSMTState;
    use crate::operator::utils::instruction_util::WithdrawalRemintInfo;
    use crate::operator::utils::rpc_util::{RetryConfig, RpcClientWithRetry};
    use crate::operator::utils::smt_util::SmtState;
    use crate::operator::MintCache;
    use crate::operator::ReleaseFundsBuilderWithNonce;
    use crate::storage::common::amount::TokenAmount;
    use crate::storage::common::models::{DbTransaction, TransactionType};
    use crate::storage::common::storage::mock::MockStorage;
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use borsh::BorshSerialize;
    use private_channel_escrow_program_client::errors::PrivateChannelEscrowProgramError;
    use private_channel_escrow_program_client::instructions::{
        ReleaseFundsBuilder, ResetSmtRootBuilder,
    };
    use private_channel_escrow_program_client::Instance;
    use solana_client::nonblocking::rpc_client::RpcClient;
    use solana_client::rpc_request::RpcRequest;
    use solana_keychain::Signer;
    use solana_sdk::commitment_config::CommitmentConfig;
    use solana_sdk::pubkey::Pubkey;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Semaphore;

    fn dummy_instruction() -> InstructionWithSigners {
        InstructionWithSigners {
            instructions: vec![],
            fee_payer: Pubkey::default(),
            signers: Vec::<&'static Signer>::new(),
            compute_unit_price: None,
            compute_budget: None,
        }
    }

    fn make_sender_state() -> SenderState {
        let mock = MockStorage::new();
        let storage = Arc::new(Storage::Mock(mock));
        let rpc_client = Arc::new(RpcClientWithRetry::with_retry_config(
            "http://localhost:8899".to_string(),
            RetryConfig::default(),
            CommitmentConfig::confirmed(),
        ));
        SenderState {
            rpc_client: rpc_client.clone(),
            source_rpc_client: rpc_client.clone(),
            fallback_rpc_client: None,
            storage: storage.clone(),
            instance_pda: None,
            smt_state: None,
            retry_counts: HashMap::new(),
            mint_builders: HashMap::new(),
            mint_cache: MintCache::new(storage),
            retry_max_attempts: 3,
            confirmation_poll_interval_ms: 400,
            rotation_retry_queue: Vec::new(),
            ambiguous_retry_queue: Vec::new(),
            pending_rotation: None,
            program_type: ProgramType::Escrow,
            remint_cache: HashMap::new(),
            pending_signatures: HashMap::new(),
            pending_remints: Vec::new(),
            in_flight: InFlightQueue::new(),
            semaphore: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
        }
    }

    fn make_sender_state_with_server(url: &str) -> SenderState {
        let mock = MockStorage::new();
        let storage = Arc::new(Storage::Mock(mock));
        let rpc_client = Arc::new(RpcClientWithRetry::with_retry_config(
            url.to_string(),
            RetryConfig {
                max_attempts: 1,
                base_delay: std::time::Duration::from_millis(1),
                max_delay: std::time::Duration::from_millis(1),
            },
            CommitmentConfig::confirmed(),
        ));
        SenderState {
            rpc_client: rpc_client.clone(),
            source_rpc_client: rpc_client.clone(),
            fallback_rpc_client: None,
            storage: storage.clone(),
            instance_pda: None,
            smt_state: None,
            retry_counts: HashMap::new(),
            mint_builders: HashMap::new(),
            mint_cache: MintCache::new(storage),
            retry_max_attempts: 3,
            confirmation_poll_interval_ms: 400,
            rotation_retry_queue: Vec::new(),
            ambiguous_retry_queue: Vec::new(),
            pending_rotation: None,
            program_type: ProgramType::Escrow,
            remint_cache: HashMap::new(),
            pending_signatures: HashMap::new(),
            pending_remints: Vec::new(),
            in_flight: InFlightQueue::new(),
            semaphore: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
        }
    }

    fn make_remint_info(txn_id: i64) -> WithdrawalRemintInfo {
        WithdrawalRemintInfo {
            transaction_id: txn_id,
            source_event_id: crate::operator::instruction_util::SourceEventId::new(
                &format!("remint-sig-{txn_id}"),
                0,
                None,
            ),
            trace_id: format!("trace-{txn_id}"),
            mint: solana_sdk::pubkey::Pubkey::new_unique(),
            user: solana_sdk::pubkey::Pubkey::new_unique(),
            user_ata: solana_sdk::pubkey::Pubkey::new_unique(),
            token_program: spl_token::id(),
            amount: 5000,
        }
    }

    // ── handle_permanent_failure ─────────────────────────────────────

    #[tokio::test]
    async fn permanent_failure_non_withdrawal_sends_failed_status() {
        let mut state = make_sender_state();
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        let ctx = TransactionContext {
            transaction_id: Some(42),
            withdrawal_nonce: None, // not a withdrawal
            trace_id: Some("trace-42".to_string()),
            deposit_claim_lease: None,
        };

        handle_permanent_failure(&mut state, &ctx, &storage_tx, "some error").await;

        let update = storage_rx.try_recv().expect("should receive status update");
        assert_eq!(update.transaction_id, 42);
        assert_eq!(update.status, TransactionStatus::Failed);
        assert_eq!(update.error_message.as_deref(), Some("some error"));
        assert!(update.remint_signature.is_none());
    }

    #[tokio::test]
    async fn permanent_failure_withdrawal_no_cache_sends_failed_status() {
        let mut state = make_sender_state();
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        // Withdrawal nonce but nothing in remint_cache
        let ctx = TransactionContext {
            transaction_id: Some(7),
            withdrawal_nonce: Some(99),
            trace_id: Some("trace-7".to_string()),
            deposit_claim_lease: None,
        };

        handle_permanent_failure(&mut state, &ctx, &storage_tx, "max retries").await;

        let update = storage_rx.try_recv().expect("should receive status update");
        assert_eq!(update.status, TransactionStatus::Failed);
        assert_eq!(update.error_message.as_deref(), Some("max retries"));
        assert!(update.remint_signature.is_none());
    }

    #[tokio::test]
    async fn permanent_failure_withdrawal_with_cache_defers_remint() {
        let mut state = make_sender_state();
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        // Populate remint cache and some pending signatures
        state.remint_cache.insert(5, make_remint_info(10));
        let sig = Signature::new_unique();
        state.pending_signatures.insert(
            5,
            vec![PendingSig {
                signature: sig,
                last_valid_block_height: 0,
            }],
        );

        let ctx = TransactionContext {
            transaction_id: Some(10),
            withdrawal_nonce: Some(5),
            trace_id: Some("trace-10".to_string()),
            deposit_claim_lease: None,
        };

        handle_permanent_failure(&mut state, &ctx, &storage_tx, "release_funds failed").await;

        // No immediate status update — transaction remains in PendingRemint in DB
        // until process_pending_remints resolves it after the finality window.
        assert!(
            storage_rx.try_recv().is_err(),
            "should NOT send a status update while remint is deferred"
        );

        // Entry should be in pending_remints
        assert_eq!(state.pending_remints.len(), 1);
        let entry = &state.pending_remints[0];
        assert_eq!(entry.ctx.transaction_id, Some(10));
        assert_eq!(entry.signatures.len(), 1);
        assert_eq!(entry.signatures[0].signature, sig);
        assert_eq!(entry.original_error, "release_funds failed");
        assert_eq!(entry.finality_check_attempts, 0);

        // remint_cache and pending_signatures should be drained
        assert!(!state.remint_cache.contains_key(&5));
        assert!(!state.pending_signatures.contains_key(&5));
    }

    #[tokio::test]
    async fn permanent_failure_zero_sigs_sends_manual_review() {
        let mut state = make_sender_state();
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        // With write-ahead, the only zero-signature case is a build/sign failure; it still escalates to ManualReview (a blind remint is unsafe).
        state.remint_cache.insert(5, make_remint_info(10));
        // Note: not inserting into pending_signatures

        let ctx = TransactionContext {
            transaction_id: Some(10),
            withdrawal_nonce: Some(5),
            trace_id: Some("trace-10".to_string()),
            deposit_claim_lease: None,
        };

        handle_permanent_failure(&mut state, &ctx, &storage_tx, "rpc send error").await;

        // Should go straight to ManualReview — no deferred remint
        let update = storage_rx
            .try_recv()
            .expect("should receive ManualReview status");
        assert_eq!(update.transaction_id, 10);
        assert_eq!(update.status, TransactionStatus::ManualReview);
        let err = update.error_message.as_deref().unwrap();
        assert!(
            err.contains("no signatures to verify"),
            "should mention no sigs: {err}"
        );

        // Nothing queued
        assert!(
            state.pending_remints.is_empty(),
            "should not queue deferred remint with zero sigs"
        );
    }

    // ── SMT-init errors must not mark a row Failed ────────────────

    fn release_funds_builder(txn_id: i64, nonce: u64) -> TransactionBuilder {
        TransactionBuilder::ReleaseFunds(Box::new(
            crate::operator::utils::instruction_util::ReleaseFundsBuilderWithNonce {
                builder: ReleaseFundsBuilder::new(),
                nonce,
                transaction_id: txn_id,
                trace_id: format!("trace-{txn_id}"),
                remint_info: None,
            },
        ))
    }

    /// Asserts no status update was sent (the row is left Processing, never Failed).
    fn assert_no_status_update(rx: &mut mpsc::Receiver<TransactionStatusUpdate>) {
        assert!(
            rx.try_recv().is_err(),
            "SMT-init error must not produce any status update (row stays Processing)"
        );
    }

    /// A fail-closed SMT-init error from lazy init (SmtRootMismatch, or an
    /// OperatorError::Account that is not a transient read) must leave the
    /// triggering withdrawal Processing for recovery, never Failed.
    #[tokio::test]
    async fn smt_init_error_leaves_row_processing_not_failed() {
        let ctx = withdrawal_ctx(10, 7);

        // Case 1: SmtRootMismatch.
        let mut state = make_sender_state();
        let (storage_tx, mut storage_rx) = mpsc::channel(10);
        route_builder_error(
            &mut state,
            &ctx,
            release_funds_builder(10, 7),
            &storage_tx,
            ProgramError::SmtRootMismatch {
                local_root: [0u8; 32],
                onchain_root: [1u8; 32],
            }
            .into(),
        )
        .await;
        assert_no_status_update(&mut storage_rx);

        // Case 2: OperatorError::Account that is not a transient read (missing instance).
        let (storage_tx, mut storage_rx) = mpsc::channel(10);
        route_builder_error(
            &mut state,
            &ctx,
            release_funds_builder(10, 7),
            &storage_tx,
            crate::error::AccountError::InstanceNotFound {
                instance: Pubkey::default(),
            }
            .into(),
        )
        .await;
        assert_no_status_update(&mut storage_rx);
    }

    /// A transient lazy-init read failure (RPC instance fetch AccountNotFound, or a
    /// DB nonce read error) happens before any signing, so the row provably never
    /// broadcast: it must requeue Processing → Pending for an automatic retry rather
    /// than freeze for recovery to quarantine into ManualReview.
    #[tokio::test]
    async fn smt_init_transient_error_requeues_to_pending() {
        // Case 1: OperatorError::Account(AccountNotFound) from the instance fetch.
        let mut state = make_sender_state();
        // Cached by handle_transaction_builder before init; requeue clears it.
        state.remint_cache.insert(7, make_remint_info(10));
        let storage = state.storage.clone();
        let Storage::Mock(ref mock) = *storage else {
            panic!("expected mock storage");
        };
        mock.pending_transactions
            .lock()
            .unwrap()
            .push(processing_withdrawal_row(10, 7));

        let (storage_tx, mut storage_rx) = mpsc::channel(10);
        route_builder_error(
            &mut state,
            &withdrawal_ctx(10, 7),
            release_funds_builder(10, 7),
            &storage_tx,
            crate::error::AccountError::AccountNotFound {
                pubkey: Pubkey::default(),
            }
            .into(),
        )
        .await;
        assert!(
            storage_rx.try_recv().is_err(),
            "transient init failure must not write a terminal status"
        );
        assert!(!state.remint_cache.contains_key(&7));
        let row = mock.pending_transactions.lock().unwrap()[0].clone();
        assert_eq!(row.status, TransactionStatus::Pending, "row requeued");
        assert_eq!(row.recovery_requeue_attempts, 1);

        // Case 2: OperatorError::Storage from reading the completed nonces.
        let mut state = make_sender_state();
        state.remint_cache.insert(7, make_remint_info(10));
        let storage = state.storage.clone();
        let Storage::Mock(ref mock) = *storage else {
            panic!("expected mock storage");
        };
        mock.pending_transactions
            .lock()
            .unwrap()
            .push(processing_withdrawal_row(10, 7));

        let (storage_tx, mut storage_rx) = mpsc::channel(10);
        route_builder_error(
            &mut state,
            &withdrawal_ctx(10, 7),
            release_funds_builder(10, 7),
            &storage_tx,
            crate::error::StorageError::DatabaseError {
                message: "transient".to_string(),
            }
            .into(),
        )
        .await;
        assert!(
            storage_rx.try_recv().is_err(),
            "transient init failure must not write a terminal status"
        );
        assert!(!state.remint_cache.contains_key(&7));
        let row = mock.pending_transactions.lock().unwrap()[0].clone();
        assert_eq!(row.status, TransactionStatus::Pending, "row requeued");
        assert_eq!(row.recovery_requeue_attempts, 1);
    }

    /// Once the durable requeue cap is hit, a transient init failure with no stashed
    /// signature can no longer safely retry, so it escalates to ManualReview.
    #[tokio::test]
    async fn smt_init_transient_error_at_requeue_cap_goes_to_manual_review() {
        let mut state = make_sender_state();
        state.remint_cache.insert(7, make_remint_info(10));
        let storage = state.storage.clone();
        let Storage::Mock(ref mock) = *storage else {
            panic!("expected mock storage");
        };
        let mut row = processing_withdrawal_row(10, 7);
        row.recovery_requeue_attempts = MAX_RECOVERY_REQUEUE_ATTEMPTS;
        mock.pending_transactions.lock().unwrap().push(row);

        let (storage_tx, mut storage_rx) = mpsc::channel(10);
        route_builder_error(
            &mut state,
            &withdrawal_ctx(10, 7),
            release_funds_builder(10, 7),
            &storage_tx,
            crate::error::AccountError::AccountNotFound {
                pubkey: Pubkey::default(),
            }
            .into(),
        )
        .await;

        let update = storage_rx
            .try_recv()
            .expect("at the cap, a transient init failure must escalate");
        assert_eq!(update.status, TransactionStatus::ManualReview);
    }

    /// A genuine build error (not SMT-init-class) MUST still mark the row Failed, so the exemption doesn't swallow real failures.
    #[tokio::test]
    async fn non_smt_build_error_still_marks_failed() {
        let mut state = make_sender_state();
        let (storage_tx, mut storage_rx) = mpsc::channel(10);
        route_builder_error(
            &mut state,
            &withdrawal_ctx(10, 7),
            release_funds_builder(10, 7),
            &storage_tx,
            ProgramError::InvalidBuilder {
                reason: "bad".to_string(),
            }
            .into(),
        )
        .await;

        let update = storage_rx
            .try_recv()
            .expect("non-SMT build error must send a Failed status");
        assert_eq!(update.status, TransactionStatus::Failed);
    }

    // ── handle_success ──────────────────────────────────────────────

    #[tokio::test]
    async fn success_clears_remint_cache_and_nonce_state() {
        let mut state = make_sender_state();
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        // Set up SMT state with a cached builder at nonce 3
        let mut smt = SenderSMTState {
            smt_state: SmtState::new(0),
            nonce_to_builder: HashMap::new(),
        };
        let ctx = TransactionContext {
            transaction_id: Some(50),
            withdrawal_nonce: Some(3),
            trace_id: Some("trace-50".to_string()),
            deposit_claim_lease: None,
        };
        smt.nonce_to_builder
            .insert(3, (ctx.clone(), ReleaseFundsBuilder::new()));
        state.smt_state = Some(smt);
        state.retry_counts.insert(3, 2);
        state.remint_cache.insert(3, make_remint_info(50));
        state.pending_signatures.insert(
            3,
            vec![PendingSig {
                signature: Signature::new_unique(),
                last_valid_block_height: 0,
            }],
        );

        let sig = solana_sdk::signature::Signature::new_unique();
        handle_success(&mut state, &ctx, sig, &storage_tx).await;

        // All nonce-keyed state should be cleaned up
        let smt = state.smt_state.as_ref().unwrap();
        assert!(!smt.nonce_to_builder.contains_key(&3));
        assert!(!state.retry_counts.contains_key(&3));
        assert!(
            !state.remint_cache.contains_key(&3),
            "remint_cache should be cleared on success"
        );
        assert!(
            !state.pending_signatures.contains_key(&3),
            "pending_signatures should be cleared on success"
        );

        // Should send Completed status
        let update = storage_rx.try_recv().expect("should receive status update");
        assert_eq!(update.transaction_id, 50);
        assert_eq!(update.status, TransactionStatus::Completed);
    }

    #[tokio::test]
    async fn send_and_confirm_stashes_withdrawal_signature() {
        let mut state = make_sender_state();
        let nonce = 42u64;

        // Simulate what send_and_confirm does: stash a signature
        let sig = Signature::new_unique();
        state
            .pending_signatures
            .entry(nonce)
            .or_default()
            .push(PendingSig {
                signature: sig,
                last_valid_block_height: 0,
            });

        assert!(state.pending_signatures.contains_key(&nonce));
        assert_eq!(state.pending_signatures[&nonce].len(), 1);
        assert_eq!(state.pending_signatures[&nonce][0].signature, sig);

        // Stash another (simulating a retry)
        let sig2 = Signature::new_unique();
        state
            .pending_signatures
            .entry(nonce)
            .or_default()
            .push(PendingSig {
                signature: sig2,
                last_valid_block_height: 0,
            });
        assert_eq!(state.pending_signatures[&nonce].len(), 2);
    }

    // ── write-ahead release signature ─────────────────────────────

    fn withdrawal_ctx(txn_id: i64, nonce: u64) -> TransactionContext {
        TransactionContext {
            transaction_id: Some(txn_id),
            withdrawal_nonce: Some(nonce),
            trace_id: Some(format!("trace-{txn_id}")),
            deposit_claim_lease: None,
        }
    }

    fn mock_blockhash(server: &mut mockito::ServerGuard) -> mockito::Mock {
        server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "getLatestBlockhash"
            })))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "context": {"slot": 1},
                        "value": {
                            "blockhash": "GHtXQBsoZHjzkAm2Sdm6FTyFHBCqBnLanJJhZFCFJXoe",
                            "lastValidBlockHeight": 100
                        }
                    }
                })
                .to_string(),
            )
            .create()
    }

    fn mock_get_signature_statuses_null(server: &mut mockito::ServerGuard) -> mockito::Mock {
        server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "getSignatureStatuses"
            })))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {"context": {"slot": 1}, "value": [null]}
                })
                .to_string(),
            )
            .create()
    }

    /// A successful send_and_confirm persists the signed transaction's signature (via `insert_release_signature`) before the broadcast.
    #[tokio::test]
    async fn release_persists_signature_before_send() {
        let mut server = mockito::Server::new_async().await;
        let _hash = mock_blockhash(&mut server);
        let _send = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "sendTransaction"
            })))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": Signature::default().to_string()
                })
                .to_string(),
            )
            .create();
        // Confirmation polls return null (Retry), but the persist already happened.
        let _status = mock_get_signature_statuses_null(&mut server);

        let mut state = make_sender_state_with_server(&server.url());
        let ctx = withdrawal_ctx(10, 5);

        send_and_confirm(
            &mut state,
            dummy_instruction(),
            None,
            &ctx,
            RetryPolicy::Idempotent,
            &ExtraErrorCheckPolicy::None,
            &mpsc::channel(10).0,
        )
        .await;

        let Storage::Mock(ref mock) = *state.storage else {
            panic!("expected mock storage");
        };
        let stored = mock.get_release_signatures(10).await.unwrap();
        assert_eq!(stored.len(), 1, "exactly one release signature persisted");
        assert_eq!(
            stored[0].0,
            Signature::default().to_string(),
            "persisted signature must be the signed transaction's signature"
        );
        assert_eq!(stored[0].1, 100, "persisted lvbh must match the blockhash");
    }

    /// A failed write-ahead persist must NOT broadcast, must write no terminal status (row
    /// left Processing), and must stash nothing. With nothing ever broadcast for this nonce
    /// the Stage-1 SMT/builder/retry/remint mutations must also roll back so later
    /// withdrawals in this tree build on a root the chain agrees with.
    #[tokio::test]
    async fn release_aborts_send_when_persist_fails() {
        let txn_id = 10;
        let nonce = 5;
        let mut server = mockito::Server::new_async().await;
        let _hash = mock_blockhash(&mut server);
        // sendTransaction must never be called once persist fails.
        let send = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "sendTransaction"
            })))
            .expect(0)
            .create();

        let mut state = make_sender_state_with_server(&server.url());
        let mut smt = SenderSMTState {
            smt_state: SmtState::new(0),
            nonce_to_builder: HashMap::new(),
        };
        smt.smt_state.insert_nonce(nonce);
        smt.nonce_to_builder.insert(
            nonce,
            (withdrawal_ctx(txn_id, nonce), ReleaseFundsBuilder::new()),
        );
        state.smt_state = Some(smt);
        state.remint_cache.insert(nonce, make_remint_info(txn_id));

        let Storage::Mock(ref mock) = *state.storage else {
            panic!("expected mock storage");
        };
        mock.pending_transactions
            .lock()
            .unwrap()
            .push(processing_withdrawal_row(txn_id, nonce));
        mock.set_should_fail("insert_release_signature", true);

        let (storage_tx, mut storage_rx) = mpsc::channel(10);
        let ctx = withdrawal_ctx(txn_id, nonce);

        send_and_confirm(
            &mut state,
            dummy_instruction(),
            None,
            &ctx,
            RetryPolicy::Idempotent,
            &ExtraErrorCheckPolicy::None,
            &storage_tx,
        )
        .await;

        send.assert();
        assert!(
            storage_rx.try_recv().is_err(),
            "no status update must be sent; row stays Processing for recovery"
        );
        assert!(
            !state.pending_signatures.contains_key(&nonce),
            "nothing stashed when persist failed"
        );

        let smt = state.smt_state.as_ref().unwrap();
        assert!(
            !smt.smt_state.contains_nonce(nonce),
            "SMT nonce rolled back when nothing broadcast"
        );
        assert!(
            !smt.nonce_to_builder.contains_key(&nonce),
            "builder cache rolled back"
        );
        assert!(
            !state.remint_cache.contains_key(&nonce),
            "remint cache rolled back"
        );
        assert!(
            !state.retry_counts.contains_key(&nonce),
            "retry count rolled back"
        );

        let Storage::Mock(ref mock) = *state.storage else {
            panic!("expected mock storage");
        };
        let row = mock.pending_transactions.lock().unwrap()[0].clone();
        assert_eq!(
            row.status,
            TransactionStatus::Processing,
            "row stays Processing for recovery, no requeue"
        );
    }

    /// Guards D1: a signature stashed from an earlier broadcast of this nonce means a real
    /// tx may still land, so a later persist failure must leave the Stage-1 state intact
    /// for recovery instead of rolling a still-in-flight nonce out of the SMT.
    #[tokio::test]
    async fn persist_failure_with_prior_broadcast_keeps_state() {
        let txn_id = 10;
        let nonce = 5;
        let mut server = mockito::Server::new_async().await;
        let _hash = mock_blockhash(&mut server);
        let send = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "sendTransaction"
            })))
            .expect(0)
            .create();

        let mut state = make_sender_state_with_server(&server.url());
        let mut smt = SenderSMTState {
            smt_state: SmtState::new(0),
            nonce_to_builder: HashMap::new(),
        };
        smt.smt_state.insert_nonce(nonce);
        smt.nonce_to_builder.insert(
            nonce,
            (withdrawal_ctx(txn_id, nonce), ReleaseFundsBuilder::new()),
        );
        state.smt_state = Some(smt);
        let prior_sig = Signature::new_unique();
        state.pending_signatures.insert(
            nonce,
            vec![PendingSig {
                signature: prior_sig,
                last_valid_block_height: 0,
            }],
        );

        let Storage::Mock(ref mock) = *state.storage else {
            panic!("expected mock storage");
        };
        mock.set_should_fail("insert_release_signature", true);

        let (storage_tx, mut storage_rx) = mpsc::channel(10);
        let ctx = withdrawal_ctx(txn_id, nonce);

        send_and_confirm(
            &mut state,
            dummy_instruction(),
            None,
            &ctx,
            RetryPolicy::Idempotent,
            &ExtraErrorCheckPolicy::None,
            &storage_tx,
        )
        .await;

        send.assert();
        assert!(
            storage_rx.try_recv().is_err(),
            "no status update; row stays Processing for recovery"
        );

        let smt = state.smt_state.as_ref().unwrap();
        assert!(
            smt.smt_state.contains_nonce(nonce),
            "nonce kept in SMT while an earlier broadcast may still land"
        );
        assert!(
            smt.nonce_to_builder.contains_key(&nonce),
            "builder cache kept for the in-flight nonce"
        );
        // The stashed signature is the invariant the guard protects: recovery
        // reconciles the in-flight nonce against it, so it must survive the abort.
        let stashed = state
            .pending_signatures
            .get(&nonce)
            .expect("stashed signature preserved for recovery to reconcile");
        assert_eq!(stashed.len(), 1, "no signatures added or dropped");
        assert_eq!(
            stashed[0].signature, prior_sig,
            "the pre-broadcast signature is intact"
        );
    }

    /// Guards against over-cleaning: a persist that succeeds must still broadcast, persist
    /// the write-ahead signature, and keep the chain-accepted nonce in the SMT on a
    /// confirmed release. Pins that the abort-branch rollback never touches the success path.
    #[tokio::test]
    async fn persist_success_still_broadcasts_and_keeps_nonce() {
        let txn_id = 10;
        let nonce = 5;
        let mut server = mockito::Server::new_async().await;
        let _hash = mock_blockhash(&mut server);
        let _send = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "sendTransaction"
            })))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": Signature::default().to_string()
                })
                .to_string(),
            )
            .create();
        // A confirmed status routes to handle_success instead of the idempotent retry loop.
        let _status = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "getSignatureStatuses"
            })))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "context": {"slot": 100},
                        "value": [{
                            "confirmationStatus": "finalized",
                            "confirmations": null,
                            "err": null,
                            "slot": 100,
                            "status": {"Ok": null}
                        }]
                    }
                })
                .to_string(),
            )
            .create();

        let mut state = make_sender_state_with_server(&server.url());
        let mut smt = SenderSMTState {
            smt_state: SmtState::new(0),
            nonce_to_builder: HashMap::new(),
        };
        smt.smt_state.insert_nonce(nonce);
        state.smt_state = Some(smt);

        let (storage_tx, mut storage_rx) = mpsc::channel(10);
        let ctx = withdrawal_ctx(txn_id, nonce);

        send_and_confirm(
            &mut state,
            dummy_instruction(),
            None,
            &ctx,
            RetryPolicy::Idempotent,
            &ExtraErrorCheckPolicy::None,
            &storage_tx,
        )
        .await;

        let smt = state.smt_state.as_ref().unwrap();
        assert!(
            smt.smt_state.contains_nonce(nonce),
            "the chain-accepted nonce must stay in the SMT on a confirmed release"
        );

        let Storage::Mock(ref mock) = *state.storage else {
            panic!("expected mock storage");
        };
        let stored = mock.get_release_signatures(txn_id).await.unwrap();
        assert_eq!(
            stored.len(),
            1,
            "the write-ahead signature is persisted on the success path"
        );

        let update = storage_rx
            .try_recv()
            .expect("a confirmed release emits a status update");
        assert_eq!(update.status, TransactionStatus::Completed);
    }

    /// The in-memory stash happens only after a successful broadcast, so a send that
    /// never reached the network leaves no signature to verify and routes to
    /// ManualReview, not a deferred remint. The write-ahead DB persist (for crash
    /// recovery) does not change this.
    #[tokio::test]
    async fn send_failure_routes_to_manual_review() {
        let mut server = mockito::Server::new_async().await;
        let _hash = mock_blockhash(&mut server);
        let _send = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "sendTransaction"
            })))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "error": {"code": -32600, "message": "Internal error"}
                })
                .to_string(),
            )
            .create();

        let mut state = make_sender_state_with_server(&server.url());
        state.remint_cache.insert(5, make_remint_info(10));

        let (storage_tx, mut storage_rx) = mpsc::channel(10);
        let ctx = withdrawal_ctx(10, 5);

        send_and_confirm(
            &mut state,
            dummy_instruction(),
            None,
            &ctx,
            RetryPolicy::Idempotent,
            &ExtraErrorCheckPolicy::None,
            &storage_tx,
        )
        .await;

        assert!(
            state.pending_remints.is_empty(),
            "a never-broadcast send must not defer a remint"
        );
        let update = storage_rx
            .try_recv()
            .expect("send failure must surface a status update");
        assert_eq!(update.status, TransactionStatus::ManualReview);
    }

    // ── pre-broadcast build/sign failure requeues withdrawals ────────

    fn processing_withdrawal_row(txn_id: i64, nonce: u64) -> DbTransaction {
        let now = Utc::now();
        DbTransaction {
            id: txn_id,
            signature: format!("sig-{txn_id}"),
            instruction_index: 0,
            trace_id: format!("trace-{txn_id}"),
            slot: 100,
            initiator: Pubkey::new_unique().to_string(),
            recipient: Pubkey::new_unique().to_string(),
            mint: Pubkey::new_unique().to_string(),
            amount: TokenAmount(1_000),
            memo: None,
            transaction_type: TransactionType::Withdrawal,
            withdrawal_nonce: Some(nonce as i64),
            status: TransactionStatus::Processing,
            created_at: now,
            updated_at: now,
            processed_at: None,
            counterpart_signature: None,
            remint_signatures: None,
            remint_last_valid_block_heights: None,
            pending_remint_deadline_at: None,
            finality_check_attempts: 0,
            recovery_requeue_attempts: 0,
            inner_index: None,
            landed_remint_signature: None,
        }
    }

    fn mock_blockhash_failure(server: &mut mockito::ServerGuard) -> mockito::Mock {
        server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "getLatestBlockhash"
            })))
            .with_status(500)
            .with_body("blockhash rpc down")
            .create()
    }

    /// A withdrawal build/sign failure happens before any signature exists, so the
    /// row must requeue Processing → Pending for an automatic retry: SMT rolled
    /// back, no terminal status, retry count kept so the attempt cap still binds.
    #[tokio::test]
    async fn withdrawal_build_sign_failure_requeues_to_pending() {
        let txn_id = 10;
        let nonce = 5;
        let mut server = mockito::Server::new_async().await;
        let _hash = mock_blockhash_failure(&mut server);

        let mut state = make_sender_state_with_server(&server.url());
        let mut smt = SenderSMTState {
            smt_state: SmtState::new(0),
            nonce_to_builder: HashMap::new(),
        };
        smt.smt_state.insert_nonce(nonce);
        smt.nonce_to_builder.insert(
            nonce,
            (withdrawal_ctx(txn_id, nonce), ReleaseFundsBuilder::new()),
        );
        state.smt_state = Some(smt);
        state.remint_cache.insert(nonce, make_remint_info(txn_id));

        let storage = state.storage.clone();
        let Storage::Mock(ref mock) = *storage else {
            panic!("expected mock storage");
        };
        mock.pending_transactions
            .lock()
            .unwrap()
            .push(processing_withdrawal_row(txn_id, nonce));

        let (storage_tx, mut storage_rx) = mpsc::channel(10);
        send_and_confirm(
            &mut state,
            dummy_instruction(),
            None,
            &withdrawal_ctx(txn_id, nonce),
            RetryPolicy::Idempotent,
            &ExtraErrorCheckPolicy::None,
            &storage_tx,
        )
        .await;

        assert!(
            storage_rx.try_recv().is_err(),
            "pre-broadcast failure must not write a terminal status"
        );
        assert!(state.pending_remints.is_empty(), "no deferred remint");

        let smt = state.smt_state.as_ref().unwrap();
        assert!(
            !smt.smt_state.contains_nonce(nonce),
            "nonce rolled back from local SMT"
        );
        assert!(!smt.nonce_to_builder.contains_key(&nonce));
        assert!(!state.remint_cache.contains_key(&nonce));
        assert_eq!(
            state.retry_counts.get(&nonce),
            Some(&1),
            "retry count survives the requeue so the attempt cap still binds"
        );

        let row = mock.pending_transactions.lock().unwrap()[0].clone();
        assert_eq!(
            row.status,
            TransactionStatus::Pending,
            "row requeued for the fetcher"
        );
        assert_eq!(row.recovery_requeue_attempts, 1);
    }

    /// With a signature stashed from an earlier broadcast of the same nonce, a
    /// later build/sign failure is not provably pre-broadcast: it must take the
    /// finality-checked remint path, not the requeue.
    #[tokio::test]
    async fn build_sign_failure_with_prior_broadcast_defers_remint() {
        let txn_id = 10;
        let nonce = 5;
        let mut server = mockito::Server::new_async().await;
        let _hash = mock_blockhash_failure(&mut server);

        let mut state = make_sender_state_with_server(&server.url());
        state.remint_cache.insert(nonce, make_remint_info(txn_id));
        let prior_sig = Signature::new_unique();
        state.pending_signatures.insert(
            nonce,
            vec![PendingSig {
                signature: prior_sig,
                last_valid_block_height: 0,
            }],
        );

        let (storage_tx, mut storage_rx) = mpsc::channel(10);
        send_and_confirm(
            &mut state,
            dummy_instruction(),
            None,
            &withdrawal_ctx(txn_id, nonce),
            RetryPolicy::Idempotent,
            &ExtraErrorCheckPolicy::None,
            &storage_tx,
        )
        .await;

        assert!(
            storage_rx.try_recv().is_err(),
            "remint is deferred, no status update yet"
        );
        assert_eq!(
            state.pending_remints.len(),
            1,
            "prior broadcast must defer a remint, not requeue"
        );
        assert_eq!(state.pending_remints[0].signatures[0].signature, prior_sig);
    }

    /// If the requeue write fails the row stays Processing: recovery quarantines
    /// no-signature withdrawals, so the failure still pages instead of stranding.
    #[tokio::test]
    async fn build_sign_failure_requeue_write_error_leaves_processing() {
        let txn_id = 10;
        let nonce = 5;
        let mut server = mockito::Server::new_async().await;
        let _hash = mock_blockhash_failure(&mut server);

        let mut state = make_sender_state_with_server(&server.url());
        let mut smt = SenderSMTState {
            smt_state: SmtState::new(0),
            nonce_to_builder: HashMap::new(),
        };
        smt.smt_state.insert_nonce(nonce);
        state.smt_state = Some(smt);
        state.remint_cache.insert(nonce, make_remint_info(txn_id));

        let storage = state.storage.clone();
        let Storage::Mock(ref mock) = *storage else {
            panic!("expected mock storage");
        };
        mock.pending_transactions
            .lock()
            .unwrap()
            .push(processing_withdrawal_row(txn_id, nonce));
        mock.set_should_fail("try_requeue_prebroadcast", true);

        let (storage_tx, mut storage_rx) = mpsc::channel(10);
        send_and_confirm(
            &mut state,
            dummy_instruction(),
            None,
            &withdrawal_ctx(txn_id, nonce),
            RetryPolicy::Idempotent,
            &ExtraErrorCheckPolicy::None,
            &storage_tx,
        )
        .await;

        assert!(
            storage_rx.try_recv().is_err(),
            "no terminal status on requeue write failure"
        );
        assert!(state.pending_remints.is_empty());
        let smt = state.smt_state.as_ref().unwrap();
        assert!(
            !smt.smt_state.contains_nonce(nonce),
            "SMT rollback happens regardless of the requeue write"
        );

        let row = mock.pending_transactions.lock().unwrap()[0].clone();
        assert_eq!(
            row.status,
            TransactionStatus::Processing,
            "row left Processing for recovery"
        );
    }

    /// A row already at the durable requeue cap must not requeue again on a
    /// build/sign failure: it pages via ManualReview instead of ping-ponging
    /// Pending ↔ Processing forever across restarts.
    #[tokio::test]
    async fn build_sign_failure_at_requeue_cap_goes_to_manual_review() {
        let txn_id = 10;
        let nonce = 5;
        let mut server = mockito::Server::new_async().await;
        let _hash = mock_blockhash_failure(&mut server);

        let mut state = make_sender_state_with_server(&server.url());
        let mut smt = SenderSMTState {
            smt_state: SmtState::new(0),
            nonce_to_builder: HashMap::new(),
        };
        smt.smt_state.insert_nonce(nonce);
        state.smt_state = Some(smt);
        state.remint_cache.insert(nonce, make_remint_info(txn_id));

        let storage = state.storage.clone();
        let Storage::Mock(ref mock) = *storage else {
            panic!("expected mock storage");
        };
        let mut row = processing_withdrawal_row(txn_id, nonce);
        row.recovery_requeue_attempts = MAX_RECOVERY_REQUEUE_ATTEMPTS;
        mock.pending_transactions.lock().unwrap().push(row);

        let (storage_tx, mut storage_rx) = mpsc::channel(10);
        send_and_confirm(
            &mut state,
            dummy_instruction(),
            None,
            &withdrawal_ctx(txn_id, nonce),
            RetryPolicy::Idempotent,
            &ExtraErrorCheckPolicy::None,
            &storage_tx,
        )
        .await;

        let update = storage_rx
            .try_recv()
            .expect("cap hit must surface a status update");
        assert_eq!(update.status, TransactionStatus::ManualReview);
        assert!(update
            .error_message
            .as_deref()
            .unwrap_or("")
            .contains("requeues"));

        let row = mock.pending_transactions.lock().unwrap()[0].clone();
        assert_eq!(
            row.status,
            TransactionStatus::Processing,
            "row must not requeue past the cap"
        );
        let smt = state.smt_state.as_ref().unwrap();
        assert!(
            !smt.smt_state.contains_nonce(nonce),
            "permanent-failure cleanup rolls back the SMT nonce"
        );
    }

    // ── set_pending_remint persistence ───────────────────────────────

    /// When a withdrawal fails permanently and is eligible for remint,
    /// `handle_permanent_failure` must persist the PendingRemint state to
    /// the database before queuing the entry in memory.
    ///
    /// This test verifies three things that are critical for crash safety:
    ///   1. `set_pending_remint` is called exactly once with the correct transaction_id.
    ///   2. All withdrawal signatures are stored — missing even one could cause a
    ///      false "not finalized" result on recovery, leading to a duplicate remint.
    ///   3. The deadline is ~32s in the future so recovery restores the correct wait
    ///      time rather than firing the remint immediately on restart.
    #[tokio::test]
    async fn permanent_failure_calls_set_pending_remint_with_correct_args() {
        let mut state = make_sender_state();
        let (storage_tx, _storage_rx) = mpsc::channel(10);

        // Two signatures — simulating a withdrawal that was retried once before
        // failing permanently. Both must be persisted for a complete finality check.
        let sig1 = Signature::new_unique();
        let sig2 = Signature::new_unique();
        let sig1_lvbh: u64 = 100;
        let sig2_lvbh: u64 = 200;
        state.remint_cache.insert(5, make_remint_info(10));
        state.pending_signatures.insert(
            5,
            vec![
                PendingSig {
                    signature: sig1,
                    last_valid_block_height: sig1_lvbh,
                },
                PendingSig {
                    signature: sig2,
                    last_valid_block_height: sig2_lvbh,
                },
            ],
        );

        let ctx = TransactionContext {
            transaction_id: Some(10),
            withdrawal_nonce: Some(5),
            trace_id: Some("trace-10".to_string()),
            deposit_claim_lease: None,
        };

        let before = Utc::now();
        handle_permanent_failure(&mut state, &ctx, &storage_tx, "release_funds failed").await;
        let after = Utc::now();

        // Extract the mock to inspect what was written to storage.
        let Storage::Mock(ref mock) = *state.storage else {
            panic!("expected mock storage");
        };
        let calls = mock.pending_remint_signatures.lock().unwrap();

        assert_eq!(
            calls.len(),
            1,
            "set_pending_remint should be called exactly once"
        );

        let (stored_id, stored_sigs, stored_lvbhs, stored_deadline) = &calls[0];
        assert_eq!(*stored_id, 10, "wrong transaction_id persisted");

        assert_eq!(
            stored_sigs.len(),
            2,
            "both withdrawal signatures must be persisted"
        );
        assert!(
            stored_sigs.contains(&sig1.to_string()),
            "sig1 must be persisted"
        );
        assert!(
            stored_sigs.contains(&sig2.to_string()),
            "sig2 must be persisted"
        );

        // lvbh array must be index-paired with sig array and carry the values
        // we stashed at send time. Otherwise the remint gate can't tell a still-
        // live broadcast from a dead one.
        assert_eq!(
            stored_sigs.len(),
            stored_lvbhs.len(),
            "sig array and lvbh array must be the same length"
        );
        let sig1_idx = stored_sigs
            .iter()
            .position(|stored_sig| stored_sig == &sig1.to_string())
            .unwrap();
        let sig2_idx = stored_sigs
            .iter()
            .position(|stored_sig| stored_sig == &sig2.to_string())
            .unwrap();
        assert_eq!(
            stored_lvbhs[sig1_idx], sig1_lvbh as i64,
            "sig1's lvbh must be persisted"
        );
        assert_eq!(
            stored_lvbhs[sig2_idx], sig2_lvbh as i64,
            "sig2's lvbh must be persisted"
        );

        // Deadline must be ~FINALITY_SAFETY_DELAY (32s) from now.
        // We allow a ±3s window to absorb test execution time.
        let expected_min = before + chrono::Duration::seconds(29);
        let expected_max = after + chrono::Duration::seconds(35);
        assert!(
            *stored_deadline >= expected_min && *stored_deadline <= expected_max,
            "deadline should be ~32s from now, got {stored_deadline}"
        );
    }

    /// When the database write for `set_pending_remint` fails, the operator
    /// cannot safely defer the remint — it has no guarantee the state will
    /// survive a restart. Instead of silently losing the remint, it must
    /// immediately escalate to ManualReview so an operator can intervene.
    ///
    /// Equally important: nothing should be queued in `pending_remints`.
    /// Queuing in memory without the DB write would be a half-written state —
    /// the entry would disappear on the next crash, violating the atomicity
    /// invariant.
    #[tokio::test]
    async fn permanent_failure_sends_manual_review_when_storage_fails() {
        let mut state = make_sender_state();
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        // Instruct the mock to fail on set_pending_remint.
        let Storage::Mock(ref mock) = *state.storage else {
            panic!("expected mock storage");
        };
        mock.set_should_fail("set_pending_remint", true);

        state.remint_cache.insert(5, make_remint_info(10));
        state.pending_signatures.insert(
            5,
            vec![PendingSig {
                signature: Signature::new_unique(),
                last_valid_block_height: 0,
            }],
        );

        let ctx = TransactionContext {
            transaction_id: Some(10),
            withdrawal_nonce: Some(5),
            trace_id: Some("trace-10".to_string()),
            deposit_claim_lease: None,
        };

        handle_permanent_failure(&mut state, &ctx, &storage_tx, "release_funds failed").await;

        // Must escalate to ManualReview — human intervention is needed.
        let update = storage_rx
            .try_recv()
            .expect("should receive ManualReview status");
        assert_eq!(update.transaction_id, 10);
        assert_eq!(update.status, TransactionStatus::ManualReview);

        // Must not queue in memory — no DB write means no crash safety.
        assert!(
            state.pending_remints.is_empty(),
            "should not queue pending remint when storage write failed"
        );
    }

    /// `send_fatal_error` must emit a `Failed` status update with the exact error message
    /// and no counterpart signature when the context contains a transaction id.
    #[tokio::test]
    async fn send_fatal_error_with_transaction_id_sends_failed_status() {
        let (tx, mut rx) = mpsc::channel(10);
        let ctx = TransactionContext {
            transaction_id: Some(42),
            withdrawal_nonce: None,
            trace_id: Some("trace-1".to_string()),
            deposit_claim_lease: None,
        };

        send_fatal_error(&tx, &ctx, "test error").await;

        let update = rx.recv().await.unwrap();
        assert_eq!(update.transaction_id, 42);
        assert_eq!(update.status, TransactionStatus::Failed);
        assert!(update.counterpart_signature.is_none());
        assert_eq!(update.error_message.as_deref(), Some("test error"));
    }

    /// Without a transaction id there is nothing to mark as failed, so `send_fatal_error`
    /// must silently drop the error and send nothing to the storage channel.
    #[tokio::test]
    async fn send_fatal_error_without_transaction_id_sends_nothing() {
        let (tx, mut rx) = mpsc::channel(10);
        let ctx = TransactionContext {
            transaction_id: None,
            withdrawal_nonce: None,
            trace_id: None,
            deposit_claim_lease: None,
        };

        send_fatal_error(&tx, &ctx, "test error").await;

        drop(tx);
        assert!(rx.recv().await.is_none());
    }

    /// A successful mint (no withdrawal nonce) must emit `Completed` with the on-chain
    /// signature as `counterpart_signature`.
    #[tokio::test]
    async fn handle_success_mint_transaction_sends_completed_status() {
        let mut state = make_sender_state();
        let (tx, mut rx) = mpsc::channel(10);
        let ctx = TransactionContext {
            transaction_id: Some(7),
            withdrawal_nonce: None,
            trace_id: Some("trace-mint".to_string()),
            deposit_claim_lease: None,
        };
        let sig = Signature::new_unique();

        handle_success(&mut state, &ctx, sig, &tx).await;

        let update = rx.recv().await.unwrap();
        assert_eq!(update.transaction_id, 7);
        assert_eq!(update.status, TransactionStatus::Completed);
        assert_eq!(
            update.counterpart_signature.as_deref(),
            Some(sig.to_string().as_str())
        );
    }

    /// A confirmed ResetSmtRoot transaction (no transaction_id, no nonce) must advance
    /// the tree index, disarm the rotation it just proved landed, and send no status
    /// update to the storage channel.
    #[tokio::test]
    async fn handle_success_reset_smt_root_increments_tree_index() {
        let mut state = make_sender_state();
        // Set up SMT state
        state.smt_state = Some(super::super::types::SenderSMTState {
            smt_state: crate::operator::utils::smt_util::SmtState::new(0),
            nonce_to_builder: HashMap::new(),
        });
        state.pending_rotation = Some(Box::new(ResetSmtRootBuilder::new()));

        let (tx, mut rx) = mpsc::channel(10);
        // No transaction_id, no withdrawal_nonce = ResetSmtRoot context
        let ctx = TransactionContext {
            transaction_id: None,
            withdrawal_nonce: None,
            trace_id: None,
            deposit_claim_lease: None,
        };
        let sig = Signature::new_unique();

        handle_success(&mut state, &ctx, sig, &tx).await;

        // No status update sent for ResetSmtRoot
        drop(tx);
        assert!(rx.recv().await.is_none());

        // Tree index should be incremented
        assert_eq!(state.smt_state.as_ref().unwrap().smt_state.tree_index(), 1);
        assert!(
            state.pending_rotation.is_none(),
            "a confirmed reset proves the tree advanced, so the rotation is disarmed"
        );
    }

    /// After a successful withdrawal, the per-nonce retry counter must be removed so that
    /// a future submission with the same nonce starts from a clean slate.
    #[tokio::test]
    async fn handle_success_withdrawal_cleans_up_nonce_state() {
        let mut state = make_sender_state();
        state.instance_pda = Some(Pubkey::new_unique());
        state.smt_state = Some(super::super::types::SenderSMTState {
            smt_state: crate::operator::utils::smt_util::SmtState::new(0),
            nonce_to_builder: HashMap::new(),
        });
        state.retry_counts.insert(5, 2);

        let (tx, mut rx) = mpsc::channel(10);
        let ctx = TransactionContext {
            transaction_id: Some(99),
            withdrawal_nonce: Some(5),
            trace_id: Some("trace-wd".to_string()),
            deposit_claim_lease: None,
        };
        let sig = Signature::new_unique();

        handle_success(&mut state, &ctx, sig, &tx).await;

        let update = rx.recv().await.unwrap();
        assert_eq!(update.transaction_id, 99);
        assert_eq!(update.status, TransactionStatus::Completed);

        // Retry count should be cleaned up
        assert!(!state.retry_counts.contains_key(&5));
    }

    // ============================================================
    // handle_confirmation_result tests (code paths that don't need RPC)
    // ============================================================

    /// `InvalidTransactionNonceForCurrentTreeIndex` is a permanent on-chain rejection; the
    /// transaction must be marked Failed and the error message must mention "nonce".
    #[tokio::test]
    async fn confirmation_result_invalid_nonce_for_tree_index_sends_fatal_error() {
        let mut state = make_sender_state();
        let (tx, mut rx) = mpsc::channel(10);
        let ctx = TransactionContext {
            transaction_id: Some(10),
            withdrawal_nonce: None,
            trace_id: None,
            deposit_claim_lease: None,
        };

        handle_confirmation_result(
            &mut state,
            Ok(ConfirmationResult::Failed(Some(
                PrivateChannelEscrowProgramError::InvalidTransactionNonceForCurrentTreeIndex,
            ))),
            Signature::new_unique(),
            None,
            &ctx,
            dummy_instruction(),
            RetryPolicy::None,
            &ExtraErrorCheckPolicy::None,
            &tx,
        )
        .await;

        let update = rx.recv().await.unwrap();
        assert_eq!(update.transaction_id, 10);
        assert_eq!(update.status, TransactionStatus::Failed);
        assert!(update
            .error_message
            .as_deref()
            .unwrap_or("")
            .contains("nonce"));
    }

    /// An unrecognised program error (None variant) is treated as a permanent failure;
    /// the transaction must be marked Failed with no retry attempt.
    #[tokio::test]
    async fn confirmation_result_other_program_error_sends_fatal_error() {
        let mut state = make_sender_state();
        let (tx, mut rx) = mpsc::channel(10);
        let ctx = TransactionContext {
            transaction_id: Some(11),
            withdrawal_nonce: None,
            trace_id: None,
            deposit_claim_lease: None,
        };

        handle_confirmation_result(
            &mut state,
            Ok(ConfirmationResult::Failed(None)),
            Signature::new_unique(),
            None,
            &ctx,
            dummy_instruction(),
            RetryPolicy::None,
            &ExtraErrorCheckPolicy::None,
            &tx,
        )
        .await;

        let update = rx.recv().await.unwrap();
        assert_eq!(update.transaction_id, 11);
        assert_eq!(update.status, TransactionStatus::Failed);
    }

    /// A reset rejected with UnexpectedTreeIndex means a reset already landed on-chain.
    /// The sender must re-fetch the tree index, sync local SMT to it, disarm the
    /// rotation now that the chain is past the bound index, and write nothing to the
    /// storage channel (a reset has no DB row).
    #[tokio::test]
    async fn confirmation_result_unexpected_tree_index_resyncs_local_smt() {
        let local_index = 4u64;
        let onchain_index = 5u64;

        let instance = Instance {
            discriminator: 0,
            bump: 0,
            version: 0,
            instance_seed: Pubkey::new_unique(),
            admin: Pubkey::new_unique(),
            withdrawal_transactions_root: [0u8; 32],
            current_tree_index: onchain_index,
        };
        let mut instance_bytes = Vec::new();
        instance.serialize(&mut instance_bytes).unwrap();

        let account_response = serde_json::json!({
            "context": {"slot": 1},
            "value": {
                "owner": Pubkey::new_unique().to_string(),
                "lamports": 1_000_000u64,
                "data": [STANDARD.encode(&instance_bytes), "base64"],
                "executable": false,
                "rentEpoch": 0
            }
        });
        let mut mocks = HashMap::new();
        mocks.insert(RpcRequest::GetAccountInfo, account_response);

        let mut state = make_sender_state();
        state.instance_pda = Some(Pubkey::new_unique());
        state.rpc_client = Arc::new(RpcClientWithRetry {
            rpc_client: Arc::new(RpcClient::new_mock_with_mocks(
                "http://127.0.0.1:8899".to_string(),
                mocks,
            )),
            retry_config: RetryConfig::default(),
        });
        state.smt_state = Some(SenderSMTState {
            smt_state: SmtState::new(local_index),
            nonce_to_builder: HashMap::new(),
        });
        state.pending_rotation = Some(Box::new(ResetSmtRootBuilder::new()));

        let (tx, mut rx) = mpsc::channel(10);
        let ctx = TransactionContext {
            transaction_id: None,
            withdrawal_nonce: None,
            trace_id: None,
            deposit_claim_lease: None,
        };

        handle_confirmation_result(
            &mut state,
            Ok(ConfirmationResult::Failed(Some(
                PrivateChannelEscrowProgramError::UnexpectedTreeIndex,
            ))),
            Signature::new_unique(),
            None,
            &ctx,
            dummy_instruction(),
            RetryPolicy::Idempotent,
            &ExtraErrorCheckPolicy::None,
            &tx,
        )
        .await;

        assert_eq!(
            state.smt_state.as_ref().unwrap().smt_state.tree_index(),
            onchain_index
        );
        assert!(
            state.pending_rotation.is_none(),
            "chain is past the bound index, so the rotation is no longer owed"
        );
        drop(tx);
        assert!(
            rx.recv().await.is_none(),
            "no status update expected for reset"
        );
    }

    /// If the on-chain re-fetch fails (here: an undeserializable instance account), the
    /// sender must leave local SMT unchanged (fail-closed) rather than guessing the
    /// index, and keep the rotation armed since nothing proved the tree advanced.
    #[tokio::test]
    async fn confirmation_result_unexpected_tree_index_fetch_failure_leaves_smt_unchanged() {
        let local_index = 4u64;

        // Too-short account data so parse_instance fails after a successful fetch.
        let account_response = serde_json::json!({
            "context": {"slot": 1},
            "value": {
                "owner": Pubkey::new_unique().to_string(),
                "lamports": 1_000_000u64,
                "data": [STANDARD.encode([0u8; 4]), "base64"],
                "executable": false,
                "rentEpoch": 0
            }
        });
        let mut mocks = HashMap::new();
        mocks.insert(RpcRequest::GetAccountInfo, account_response);

        let mut state = make_sender_state();
        state.instance_pda = Some(Pubkey::new_unique());
        state.rpc_client = Arc::new(RpcClientWithRetry {
            rpc_client: Arc::new(RpcClient::new_mock_with_mocks(
                "http://127.0.0.1:8899".to_string(),
                mocks,
            )),
            retry_config: RetryConfig::default(),
        });
        state.smt_state = Some(SenderSMTState {
            smt_state: SmtState::new(local_index),
            nonce_to_builder: HashMap::new(),
        });
        state.pending_rotation = Some(Box::new(ResetSmtRootBuilder::new()));

        let (tx, mut rx) = mpsc::channel(10);
        let ctx = TransactionContext {
            transaction_id: None,
            withdrawal_nonce: None,
            trace_id: None,
            deposit_claim_lease: None,
        };

        handle_confirmation_result(
            &mut state,
            Ok(ConfirmationResult::Failed(Some(
                PrivateChannelEscrowProgramError::UnexpectedTreeIndex,
            ))),
            Signature::new_unique(),
            None,
            &ctx,
            dummy_instruction(),
            RetryPolicy::Idempotent,
            &ExtraErrorCheckPolicy::None,
            &tx,
        )
        .await;

        assert_eq!(
            state.smt_state.as_ref().unwrap().smt_state.tree_index(),
            local_index,
            "local SMT must be unchanged when re-fetch fails"
        );
        assert!(
            state.pending_rotation.is_some(),
            "nothing proved the tree advanced, so the rotation stays armed"
        );
        drop(tx);
        assert!(rx.recv().await.is_none());
    }

    /// A re-fetch that returns the index we are already on proves nothing advanced.
    /// Syncing anyway would call reset(), which clears the tree and would drop the
    /// nonces already inserted for it, so the sync must be skipped and the rotation
    /// stay armed.
    #[tokio::test]
    async fn confirmation_result_unexpected_tree_index_same_index_keeps_tree_and_rotation() {
        let tree_index = 0u64;
        let nonce = 1u64;

        let instance = Instance {
            discriminator: 0,
            bump: 0,
            version: 0,
            instance_seed: Pubkey::new_unique(),
            admin: Pubkey::new_unique(),
            withdrawal_transactions_root: [0u8; 32],
            current_tree_index: tree_index,
        };
        let mut instance_bytes = Vec::new();
        instance.serialize(&mut instance_bytes).unwrap();

        let account_response = serde_json::json!({
            "context": {"slot": 1},
            "value": {
                "owner": Pubkey::new_unique().to_string(),
                "lamports": 1_000_000u64,
                "data": [STANDARD.encode(&instance_bytes), "base64"],
                "executable": false,
                "rentEpoch": 0
            }
        });
        let mut mocks = HashMap::new();
        mocks.insert(RpcRequest::GetAccountInfo, account_response);

        let mut state = make_sender_state();
        state.instance_pda = Some(Pubkey::new_unique());
        state.rpc_client = Arc::new(RpcClientWithRetry {
            rpc_client: Arc::new(RpcClient::new_mock_with_mocks(
                "http://127.0.0.1:8899".to_string(),
                mocks,
            )),
            retry_config: RetryConfig::default(),
        });
        let mut smt = SenderSMTState {
            smt_state: SmtState::new(tree_index),
            nonce_to_builder: HashMap::new(),
        };
        smt.smt_state.insert_nonce(nonce);
        state.smt_state = Some(smt);
        state.pending_rotation = Some(Box::new(ResetSmtRootBuilder::new()));

        let (tx, mut rx) = mpsc::channel(10);
        let ctx = TransactionContext {
            transaction_id: None,
            withdrawal_nonce: None,
            trace_id: None,
            deposit_claim_lease: None,
        };

        handle_confirmation_result(
            &mut state,
            Ok(ConfirmationResult::Failed(Some(
                PrivateChannelEscrowProgramError::UnexpectedTreeIndex,
            ))),
            Signature::new_unique(),
            None,
            &ctx,
            dummy_instruction(),
            RetryPolicy::Idempotent,
            &ExtraErrorCheckPolicy::None,
            &tx,
        )
        .await;

        let smt = state.smt_state.as_ref().unwrap();
        assert_eq!(smt.smt_state.tree_index(), tree_index);
        assert!(
            smt.smt_state.contains_nonce(nonce),
            "a same-index sync must not clear the current tree"
        );
        assert!(
            state.pending_rotation.is_some(),
            "chain is not past the bound index, so the rotation stays armed"
        );
        drop(tx);
        assert!(rx.recv().await.is_none());
    }

    /// A reset that times out in confirmation must not re-send inline: it has no
    /// nonce, so send_and_confirm's attempt cap does not apply. The armed rotation is
    /// left for the rotation tick, so no RPC call is made from this path.
    #[tokio::test]
    async fn confirmation_timeout_for_reset_does_not_resend_inline() {
        let mut server = mockito::Server::new_async().await;
        // A resend starts with build_and_sign, so a blockhash read proves it happened.
        let blockhash = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "getLatestBlockhash"
            })))
            .expect(0)
            .create();

        let mut state = make_sender_state_with_server(&server.url());
        state.pending_rotation = Some(Box::new(ResetSmtRootBuilder::new()));

        let (tx, mut rx) = mpsc::channel(10);
        let ctx = TransactionContext {
            transaction_id: None,
            withdrawal_nonce: None,
            trace_id: None,
            deposit_claim_lease: None,
        };

        handle_confirmation_result(
            &mut state,
            Ok(ConfirmationResult::Retry),
            Signature::new_unique(),
            None,
            &ctx,
            dummy_instruction(),
            RetryPolicy::Idempotent,
            &ExtraErrorCheckPolicy::None,
            &tx,
        )
        .await;

        blockhash.assert();
        assert!(
            state.pending_rotation.is_some(),
            "rotation stays armed for the tick to retry"
        );
        drop(tx);
        assert!(rx.recv().await.is_none(), "a reset has no row to update");
    }

    // ── rotation arming ──────────────────────────────────────────────

    /// The rotation must be armed before the fallible SMT init, or an init failure
    /// drops it: a reset has no DB row and the fail-closed init arm writes no status,
    /// so the armed slot is the only thing that keeps the rotation owed.
    #[tokio::test]
    async fn reset_arms_rotation_before_smt_init_failure() {
        ensure_test_signer();

        // make_sender_state leaves instance_pda None, so validate_smt_root fails with
        // InstanceNotFound: init fails before the in-flight check and before any
        // instruction is built.
        let mut state = make_sender_state();

        let (storage_tx, mut storage_rx) = mpsc::channel(10);
        handle_transaction_submission(
            &mut state,
            TransactionBuilder::ResetSmtRoot(Box::new(ResetSmtRootBuilder::new())),
            &storage_tx,
        )
        .await;

        assert!(
            state.pending_rotation.is_some(),
            "rotation must survive an init failure so the tick can retry it"
        );
        assert!(
            storage_rx.try_recv().is_err(),
            "a reset has no row, so no status update"
        );
    }

    /// A `Retry` result with `RetryPolicy::None` (non-idempotent operation) cannot be safely
    /// retried, so it must be converted to a fatal failure with an "unknown" error message.
    #[tokio::test]
    async fn confirmation_result_retry_with_none_policy_sends_fatal_error() {
        let mut state = make_sender_state();
        let (tx, mut rx) = mpsc::channel(10);
        let ctx = TransactionContext {
            transaction_id: Some(12),
            withdrawal_nonce: None,
            trace_id: None,
            deposit_claim_lease: None,
        };

        handle_confirmation_result(
            &mut state,
            Ok(ConfirmationResult::Retry),
            Signature::new_unique(),
            None,
            &ctx,
            dummy_instruction(),
            RetryPolicy::None,
            &ExtraErrorCheckPolicy::None,
            &tx,
        )
        .await;

        let update = rx.recv().await.unwrap();
        assert_eq!(update.transaction_id, 12);
        assert_eq!(update.status, TransactionStatus::Failed);
        assert!(update
            .error_message
            .as_deref()
            .unwrap_or("")
            .contains("unknown"));
    }

    /// An RPC transport error bubbled up as `TransactionError::Rpc` must result in a Failed
    /// status update; the error message must contain the original RPC error text.
    #[tokio::test]
    async fn confirmation_result_rpc_error_sends_fatal_error() {
        let mut state = make_sender_state();
        let (tx, mut rx) = mpsc::channel(10);
        let ctx = TransactionContext {
            transaction_id: Some(13),
            withdrawal_nonce: None,
            trace_id: None,
            deposit_claim_lease: None,
        };

        let rpc_err = Box::new(
            solana_rpc_client_api::client_error::Error::new_with_request(
                solana_rpc_client_api::client_error::ErrorKind::Custom(
                    "test rpc error".to_string(),
                ),
                solana_rpc_client_api::request::RpcRequest::GetBalance,
            ),
        );

        handle_confirmation_result(
            &mut state,
            Err(TransactionError::Rpc(rpc_err)),
            Signature::new_unique(),
            None,
            &ctx,
            dummy_instruction(),
            RetryPolicy::None,
            &ExtraErrorCheckPolicy::None,
            &tx,
        )
        .await;

        let update = rx.recv().await.unwrap();
        assert_eq!(update.transaction_id, 13);
        assert_eq!(update.status, TransactionStatus::Failed);
        assert!(
            update
                .error_message
                .as_deref()
                .unwrap_or("")
                .contains("test rpc error"),
            "expected error message to contain RPC error text, got: {:?}",
            update.error_message
        );
    }

    /// When `MintNotInitialized` fires but no matching mint builder exists in state, the
    /// fallback path must emit a fatal error so the transaction is not silently dropped.
    #[tokio::test]
    async fn confirmation_result_mint_not_initialized_no_transaction_id_sends_fatal_error() {
        let mut state = make_sender_state();
        let (tx, mut rx) = mpsc::channel(10);
        let ctx = TransactionContext {
            transaction_id: Some(14),
            withdrawal_nonce: None,
            trace_id: None,
            deposit_claim_lease: None,
        };

        handle_confirmation_result(
            &mut state,
            Ok(ConfirmationResult::MintNotInitialized),
            Signature::new_unique(),
            None,
            &ctx,
            dummy_instruction(),
            RetryPolicy::None,
            &ExtraErrorCheckPolicy::None,
            &tx,
        )
        .await;

        // Should get a fatal error because no mint_builder in state
        let update = rx.recv().await.unwrap();
        assert_eq!(update.transaction_id, 14);
        assert_eq!(update.status, TransactionStatus::Failed);
    }

    /// `MintNotInitialized` with no transaction_id means there is nothing to report to storage;
    /// `send_fatal_error` must be a no-op and the channel must remain empty.
    #[tokio::test]
    async fn confirmation_result_mint_not_initialized_without_transaction_id() {
        let mut state = make_sender_state();
        let (tx, mut rx) = mpsc::channel(10);
        // No transaction_id
        let ctx = TransactionContext {
            transaction_id: None,
            withdrawal_nonce: None,
            trace_id: None,
            deposit_claim_lease: None,
        };

        handle_confirmation_result(
            &mut state,
            Ok(ConfirmationResult::MintNotInitialized),
            Signature::new_unique(),
            None,
            &ctx,
            dummy_instruction(),
            RetryPolicy::None,
            &ExtraErrorCheckPolicy::None,
            &tx,
        )
        .await;

        // No transaction_id → send_fatal_error sends nothing
        drop(tx);
        assert!(rx.recv().await.is_none());
    }

    /// When the per-nonce retry counter has already reached the maximum, `send_and_confirm`
    /// must short-circuit immediately with a Failed status mentioning "retries".
    #[tokio::test]
    async fn send_and_confirm_max_retries_exceeded_sends_fatal_error() {
        let mut state = make_sender_state();
        // Pre-fill retry_counts to be at max
        state.retry_counts.insert(5, 3);
        state.retry_max_attempts = 3;

        let (tx, mut rx) = mpsc::channel(10);
        let ctx = TransactionContext {
            transaction_id: Some(20),
            withdrawal_nonce: Some(5),
            trace_id: None,
            deposit_claim_lease: None,
        };

        send_and_confirm(
            &mut state,
            dummy_instruction(),
            None,
            &ctx,
            RetryPolicy::Idempotent,
            &ExtraErrorCheckPolicy::None,
            &tx,
        )
        .await;

        let update = rx.recv().await.unwrap();
        assert_eq!(update.transaction_id, 20);
        assert_eq!(update.status, TransactionStatus::Failed);
        assert!(update
            .error_message
            .as_deref()
            .unwrap_or("")
            .contains("retries"));
    }

    /// A `Confirmed` result must emit `Completed` with the on-chain signature stored as
    /// `counterpart_signature`, confirming the happy-path status-update flow.
    #[tokio::test]
    async fn confirmation_result_confirmed_sends_completed_status() {
        let mut state = make_sender_state();
        state.smt_state = Some(super::super::types::SenderSMTState {
            smt_state: crate::operator::utils::smt_util::SmtState::new(0),
            nonce_to_builder: HashMap::new(),
        });
        let (tx, mut rx) = mpsc::channel(10);
        let ctx = TransactionContext {
            transaction_id: Some(30),
            withdrawal_nonce: Some(2),
            trace_id: Some("trace-confirmed".to_string()),
            deposit_claim_lease: None,
        };
        let sig = Signature::new_unique();

        handle_confirmation_result(
            &mut state,
            Ok(ConfirmationResult::Confirmed),
            sig,
            None,
            &ctx,
            dummy_instruction(),
            RetryPolicy::Idempotent,
            &ExtraErrorCheckPolicy::None,
            &tx,
        )
        .await;

        let update = rx.recv().await.unwrap();
        assert_eq!(update.transaction_id, 30);
        assert_eq!(update.status, TransactionStatus::Completed);
        assert_eq!(
            update.counterpart_signature.as_deref(),
            Some(sig.to_string().as_str())
        );
    }

    /// `InvalidSmtProof` without a nonce means there is no builder to regenerate a proof with,
    /// so the transaction must immediately fail rather than attempt a retry.
    #[tokio::test]
    async fn confirmation_result_invalid_smt_proof_no_nonce_sends_fatal_error() {
        let mut state = make_sender_state();
        let (tx, mut rx) = mpsc::channel(10);
        let ctx = TransactionContext {
            transaction_id: Some(15),
            withdrawal_nonce: None, // No nonce → rebuild_with_regenerated_proof returns None
            trace_id: None,
            deposit_claim_lease: None,
        };

        handle_confirmation_result(
            &mut state,
            Ok(ConfirmationResult::Failed(Some(
                PrivateChannelEscrowProgramError::InvalidSmtProof,
            ))),
            Signature::new_unique(),
            None,
            &ctx,
            dummy_instruction(),
            RetryPolicy::None,
            &ExtraErrorCheckPolicy::None,
            &tx,
        )
        .await;

        let update = rx.recv().await.unwrap();
        assert_eq!(update.transaction_id, 15);
        assert_eq!(update.status, TransactionStatus::Failed);
    }

    // ── fire_and_store ────────────────────────────────────────────────

    /// A successful send must push exactly one InFlightTx with poll_attempts=0
    /// and the returned signature; no storage update must be emitted yet.
    #[tokio::test]
    async fn fire_and_store_success_pushes_to_in_flight() {
        let mut server = mockito::Server::new_async().await;

        let expected_sig = Signature::default().to_string();

        let _m_hash = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "getLatestBlockhash"
            })))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "context": {"slot": 1},
                        "value": {
                            "blockhash": "GHtXQBsoZHjzkAm2Sdm6FTyFHBCqBnLanJJhZFCFJXoe",
                            "lastValidBlockHeight": 100
                        }
                    }
                })
                .to_string(),
            )
            .create();

        let _m_send = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "sendTransaction"
            })))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": expected_sig
                })
                .to_string(),
            )
            .create();

        let mut state = {
            let storage = Arc::new(Storage::Mock(MockStorage::new()));
            SenderState {
                rpc_client: Arc::new(RpcClientWithRetry::with_retry_config(
                    server.url(),
                    crate::operator::utils::rpc_util::RetryConfig {
                        max_attempts: 1,
                        base_delay: std::time::Duration::from_millis(1),
                        max_delay: std::time::Duration::from_millis(1),
                    },
                    CommitmentConfig::confirmed(),
                )),
                source_rpc_client: Arc::new(RpcClientWithRetry::with_retry_config(
                    server.url(),
                    crate::operator::utils::rpc_util::RetryConfig {
                        max_attempts: 1,
                        base_delay: std::time::Duration::from_millis(1),
                        max_delay: std::time::Duration::from_millis(1),
                    },
                    CommitmentConfig::confirmed(),
                )),
                fallback_rpc_client: None,
                storage: storage.clone(),
                instance_pda: None,
                smt_state: None,
                retry_counts: HashMap::new(),
                mint_builders: HashMap::new(),
                mint_cache: crate::operator::MintCache::new(storage),
                retry_max_attempts: 3,
                confirmation_poll_interval_ms: 400,
                rotation_retry_queue: Vec::new(),
                ambiguous_retry_queue: Vec::new(),
                pending_rotation: None,
                program_type: ProgramType::Escrow,
                remint_cache: HashMap::new(),
                pending_signatures: HashMap::new(),
                pending_remints: Vec::new(),
                in_flight: InFlightQueue::new(),
                semaphore: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
            }
        };

        let (storage_tx, mut storage_rx) = mpsc::channel(10);
        let ctx = TransactionContext {
            transaction_id: Some(42),
            withdrawal_nonce: None,
            trace_id: Some("trace-fire".to_string()),
            deposit_claim_lease: None,
        };

        fire_and_store(
            &mut state,
            dummy_instruction(),
            None,
            ctx.clone(),
            RetryPolicy::None,
            ExtraErrorCheckPolicy::None,
            &storage_tx,
            0,
            Arc::new(Semaphore::new(MAX_IN_FLIGHT))
                .try_acquire_owned()
                .unwrap(),
        )
        .await;

        // No storage update yet — confirmation is deferred.
        assert!(
            storage_rx.try_recv().is_err(),
            "fire_and_store must not emit a status update immediately"
        );

        // Exactly one in-flight entry with the expected signature.
        assert_eq!(state.in_flight.len(), 1);
        let guard = state.in_flight.entries.lock().unwrap();
        let entry = &guard[0];
        assert_eq!(entry.signature.to_string(), expected_sig);
        assert_eq!(entry.ctx.transaction_id, Some(42));
        assert_eq!(entry.poll_attempts, 0);
    }

    /// When sendTransaction fails, fire_and_store must route to permanent failure
    /// and emit a Failed status — no in-flight entry should be added.
    #[tokio::test]
    async fn fire_and_store_send_failure_routes_to_permanent_failure() {
        let mut server = mockito::Server::new_async().await;

        // getLatestBlockhash succeeds
        let _m_hash = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "getLatestBlockhash"
            })))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "context": {"slot": 1},
                        "value": {
                            "blockhash": "GHtXQBsoZHjzkAm2Sdm6FTyFHBCqBnLanJJhZFCFJXoe",
                            "lastValidBlockHeight": 100
                        }
                    }
                })
                .to_string(),
            )
            .create();

        // sendTransaction returns an RPC error
        let _m_send = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "sendTransaction"
            })))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "error": {"code": -32600, "message": "Internal error"}
                })
                .to_string(),
            )
            .create();

        let mut state = {
            let storage = Arc::new(Storage::Mock(MockStorage::new()));
            SenderState {
                rpc_client: Arc::new(RpcClientWithRetry::with_retry_config(
                    server.url(),
                    crate::operator::utils::rpc_util::RetryConfig {
                        max_attempts: 1,
                        base_delay: std::time::Duration::from_millis(1),
                        max_delay: std::time::Duration::from_millis(1),
                    },
                    CommitmentConfig::confirmed(),
                )),
                source_rpc_client: Arc::new(RpcClientWithRetry::with_retry_config(
                    server.url(),
                    crate::operator::utils::rpc_util::RetryConfig {
                        max_attempts: 1,
                        base_delay: std::time::Duration::from_millis(1),
                        max_delay: std::time::Duration::from_millis(1),
                    },
                    CommitmentConfig::confirmed(),
                )),
                fallback_rpc_client: None,
                storage: storage.clone(),
                instance_pda: None,
                smt_state: None,
                retry_counts: HashMap::new(),
                mint_builders: HashMap::new(),
                mint_cache: crate::operator::MintCache::new(storage),
                retry_max_attempts: 3,
                confirmation_poll_interval_ms: 400,
                rotation_retry_queue: Vec::new(),
                ambiguous_retry_queue: Vec::new(),
                pending_rotation: None,
                program_type: ProgramType::Escrow,
                remint_cache: HashMap::new(),
                pending_signatures: HashMap::new(),
                pending_remints: Vec::new(),
                in_flight: InFlightQueue::new(),
                semaphore: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
            }
        };

        let (storage_tx, mut storage_rx) = mpsc::channel(10);
        let ctx = TransactionContext {
            transaction_id: Some(55),
            withdrawal_nonce: None,
            trace_id: None,
            deposit_claim_lease: None,
        };

        fire_and_store(
            &mut state,
            dummy_instruction(),
            None,
            ctx,
            RetryPolicy::None,
            ExtraErrorCheckPolicy::None,
            &storage_tx,
            0,
            Arc::new(Semaphore::new(MAX_IN_FLIGHT))
                .try_acquire_owned()
                .unwrap(),
        )
        .await;

        // Failed status must be emitted immediately.
        let update = storage_rx
            .try_recv()
            .expect("expected Failed status update");
        assert_eq!(update.transaction_id, 55);
        assert_eq!(update.status, TransactionStatus::Failed);

        // Nothing pushed to in_flight.
        assert!(
            state.in_flight.is_empty(),
            "in_flight must stay empty on send failure"
        );
    }

    // ── poll_in_flight ────────────────────────────────────────────────

    fn make_in_flight_tx(sig: Signature, txn_id: i64) -> super::super::types::InFlightTx {
        super::super::types::InFlightTx {
            signature: sig,
            ctx: TransactionContext {
                transaction_id: Some(txn_id),
                withdrawal_nonce: None,
                trace_id: Some(format!("trace-{txn_id}")),
                deposit_claim_lease: None,
            },
            instruction: dummy_instruction(),
            compute_unit_price: None,
            retry_policy: RetryPolicy::None,
            extra_error_checks_policy: ExtraErrorCheckPolicy::None,
            poll_attempts: 0,
            resend_count: 0,
            // Default to not-persisted; tests that model a write-ahead-persisted
            // deposit mint set `persisted = true` on the returned value explicitly.
            persisted: false,
            permit: Arc::new(Semaphore::new(MAX_IN_FLIGHT))
                .try_acquire_owned()
                .unwrap(),
        }
    }

    /// A confirmed signature in the batch must route to handle_success, emitting
    /// a Completed status and removing the entry from in_flight.
    #[tokio::test]
    async fn poll_in_flight_finalized_tx_emits_completed() {
        let mut server = mockito::Server::new_async().await;

        let sig = Signature::new_unique();

        let _m = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "getSignatureStatuses"
            })))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "context": {"slot": 100},
                        "value": [{
                            "confirmationStatus": "finalized",
                            "confirmations": null,
                            "err": null,
                            "slot": 100,
                            "status": {"Ok": null}
                        }]
                    }
                })
                .to_string(),
            )
            .create();

        let storage = Arc::new(Storage::Mock(MockStorage::new()));
        let mut state = SenderState {
            rpc_client: Arc::new(RpcClientWithRetry::with_retry_config(
                server.url(),
                crate::operator::utils::rpc_util::RetryConfig {
                    max_attempts: 1,
                    base_delay: std::time::Duration::from_millis(1),
                    max_delay: std::time::Duration::from_millis(1),
                },
                CommitmentConfig::confirmed(),
            )),
            source_rpc_client: Arc::new(RpcClientWithRetry::with_retry_config(
                server.url(),
                crate::operator::utils::rpc_util::RetryConfig {
                    max_attempts: 1,
                    base_delay: std::time::Duration::from_millis(1),
                    max_delay: std::time::Duration::from_millis(1),
                },
                CommitmentConfig::confirmed(),
            )),
            fallback_rpc_client: None,
            storage: storage.clone(),
            instance_pda: None,
            smt_state: None,
            retry_counts: HashMap::new(),
            mint_builders: HashMap::new(),
            mint_cache: crate::operator::MintCache::new(storage),
            retry_max_attempts: 3,
            confirmation_poll_interval_ms: 400,
            rotation_retry_queue: Vec::new(),
            ambiguous_retry_queue: Vec::new(),
            pending_rotation: None,
            program_type: ProgramType::Escrow,
            remint_cache: HashMap::new(),
            pending_signatures: HashMap::new(),
            pending_remints: Vec::new(),
            in_flight: {
                let q = InFlightQueue::new();
                q.push(make_in_flight_tx(sig, 77));
                q
            },
            semaphore: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
        };

        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        poll_in_flight(&mut state, &storage_tx).await;

        // Entry removed from in_flight after confirmation.
        assert!(
            state.in_flight.is_empty(),
            "in_flight must be empty after confirmation"
        );

        // Completed status emitted.
        let update = storage_rx.try_recv().expect("expected Completed status");
        assert_eq!(update.transaction_id, 77);
        assert_eq!(update.status, TransactionStatus::Completed);
    }

    /// A not-yet-confirmed tx should stay in in_flight with an incremented poll_attempts counter
    /// and no storage update must be emitted.
    #[tokio::test]
    async fn poll_in_flight_unconfirmed_tx_stays_in_flight() {
        let mut server = mockito::Server::new_async().await;

        let sig = Signature::new_unique();

        let _m = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "getSignatureStatuses"
            })))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "context": {"slot": 10},
                        "value": [null]   // not yet seen by RPC
                    }
                })
                .to_string(),
            )
            .create();

        let storage = Arc::new(Storage::Mock(MockStorage::new()));
        let mut state = SenderState {
            rpc_client: Arc::new(RpcClientWithRetry::with_retry_config(
                server.url(),
                crate::operator::utils::rpc_util::RetryConfig {
                    max_attempts: 1,
                    base_delay: std::time::Duration::from_millis(1),
                    max_delay: std::time::Duration::from_millis(1),
                },
                CommitmentConfig::confirmed(),
            )),
            source_rpc_client: Arc::new(RpcClientWithRetry::with_retry_config(
                server.url(),
                crate::operator::utils::rpc_util::RetryConfig {
                    max_attempts: 1,
                    base_delay: std::time::Duration::from_millis(1),
                    max_delay: std::time::Duration::from_millis(1),
                },
                CommitmentConfig::confirmed(),
            )),
            fallback_rpc_client: None,
            storage: storage.clone(),
            instance_pda: None,
            smt_state: None,
            retry_counts: HashMap::new(),
            mint_builders: HashMap::new(),
            mint_cache: crate::operator::MintCache::new(storage),
            retry_max_attempts: 3,
            confirmation_poll_interval_ms: 400,
            rotation_retry_queue: Vec::new(),
            ambiguous_retry_queue: Vec::new(),
            pending_rotation: None,
            program_type: ProgramType::Escrow,
            remint_cache: HashMap::new(),
            pending_signatures: HashMap::new(),
            pending_remints: Vec::new(),
            in_flight: {
                let q = InFlightQueue::new();
                q.push(make_in_flight_tx(sig, 88));
                q
            },
            semaphore: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
        };

        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        poll_in_flight(&mut state, &storage_tx).await;

        // Still in-flight with incremented counter.
        assert_eq!(state.in_flight.len(), 1);
        assert_eq!(state.in_flight.entries.lock().unwrap()[0].poll_attempts, 1);

        // No storage update.
        assert!(
            storage_rx.try_recv().is_err(),
            "no status update for pending tx"
        );
    }

    /// On RPC error, the entire batch must be kept in-flight untouched for retry on the
    /// next tick — poll_attempts must NOT be incremented (the RPC call did not count).
    #[tokio::test]
    async fn poll_in_flight_rpc_error_keeps_batch_unchanged() {
        let mut server = mockito::Server::new_async().await;

        let sig = Signature::new_unique();

        let _m = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "getSignatureStatuses"
            })))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "error": {"code": -32600, "message": "Internal error"}
                })
                .to_string(),
            )
            .create();

        let storage = Arc::new(Storage::Mock(MockStorage::new()));
        let mut state = SenderState {
            rpc_client: Arc::new(RpcClientWithRetry::with_retry_config(
                server.url(),
                crate::operator::utils::rpc_util::RetryConfig {
                    max_attempts: 1,
                    base_delay: std::time::Duration::from_millis(1),
                    max_delay: std::time::Duration::from_millis(1),
                },
                CommitmentConfig::confirmed(),
            )),
            source_rpc_client: Arc::new(RpcClientWithRetry::with_retry_config(
                server.url(),
                crate::operator::utils::rpc_util::RetryConfig {
                    max_attempts: 1,
                    base_delay: std::time::Duration::from_millis(1),
                    max_delay: std::time::Duration::from_millis(1),
                },
                CommitmentConfig::confirmed(),
            )),
            fallback_rpc_client: None,
            storage: storage.clone(),
            instance_pda: None,
            smt_state: None,
            retry_counts: HashMap::new(),
            mint_builders: HashMap::new(),
            mint_cache: crate::operator::MintCache::new(storage),
            retry_max_attempts: 3,
            confirmation_poll_interval_ms: 400,
            rotation_retry_queue: Vec::new(),
            ambiguous_retry_queue: Vec::new(),
            pending_rotation: None,
            program_type: ProgramType::Escrow,
            remint_cache: HashMap::new(),
            pending_signatures: HashMap::new(),
            pending_remints: Vec::new(),
            in_flight: {
                let q = InFlightQueue::new();
                q.push(make_in_flight_tx(sig, 99));
                q
            },
            semaphore: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
        };

        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        poll_in_flight(&mut state, &storage_tx).await;

        // Batch unchanged — RPC error is transient.
        assert_eq!(
            state.in_flight.len(),
            1,
            "in_flight must be unchanged on RPC error"
        );
        assert_eq!(
            state.in_flight.entries.lock().unwrap()[0].poll_attempts,
            0,
            "poll_attempts must not increment on RPC error"
        );
        assert!(
            storage_rx.try_recv().is_err(),
            "no storage update on RPC error"
        );
    }

    /// When poll_attempts reaches MAX_POLL_ATTEMPTS_CONFIRMATION for a persisted
    /// RetryPolicy::None mint, the broadcast may have landed, so it must be removed
    /// from in_flight and left Processing for recovery (no terminal Failed write).
    #[tokio::test]
    async fn poll_in_flight_timeout_persisted_mint_left_processing() {
        let mut server = mockito::Server::new_async().await;

        let sig = Signature::new_unique();

        // Return "not confirmed" enough times to trigger timeout
        let _m = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "getSignatureStatuses"
            })))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {"context": {"slot": 10}, "value": [null]}
                })
                .to_string(),
            )
            .expect(1)
            .create();

        let storage = Arc::new(Storage::Mock(MockStorage::new()));
        let mut state = SenderState {
            rpc_client: Arc::new(RpcClientWithRetry::with_retry_config(
                server.url(),
                crate::operator::utils::rpc_util::RetryConfig {
                    max_attempts: 1,
                    base_delay: std::time::Duration::from_millis(1),
                    max_delay: std::time::Duration::from_millis(1),
                },
                CommitmentConfig::confirmed(),
            )),
            source_rpc_client: Arc::new(RpcClientWithRetry::with_retry_config(
                server.url(),
                crate::operator::utils::rpc_util::RetryConfig {
                    max_attempts: 1,
                    base_delay: std::time::Duration::from_millis(1),
                    max_delay: std::time::Duration::from_millis(1),
                },
                CommitmentConfig::confirmed(),
            )),
            fallback_rpc_client: None,
            storage: storage.clone(),
            instance_pda: None,
            smt_state: None,
            retry_counts: HashMap::new(),
            mint_builders: HashMap::new(),
            mint_cache: crate::operator::MintCache::new(storage),
            retry_max_attempts: 3,
            confirmation_poll_interval_ms: 400,
            rotation_retry_queue: Vec::new(),
            ambiguous_retry_queue: Vec::new(),
            pending_rotation: None,
            program_type: ProgramType::Escrow,
            remint_cache: HashMap::new(),
            pending_signatures: HashMap::new(),
            pending_remints: Vec::new(),
            in_flight: {
                let q = InFlightQueue::new();
                let mut tx = make_in_flight_tx(sig, 101);
                // Pre-fill poll_attempts to one below MAX so this poll tips it over.
                tx.poll_attempts = MAX_POLL_ATTEMPTS_CONFIRMATION - 1;
                // A real None-policy mint reaches in_flight only after a write-ahead persist.
                tx.persisted = true;
                q.push(tx);
                q
            },
            semaphore: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
        };

        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        poll_in_flight(&mut state, &storage_tx).await;

        // Entry removed from in_flight.
        assert!(
            state.in_flight.is_empty(),
            "timed-out tx must leave in_flight"
        );

        // No terminal status: the row is left Processing for recovery to reconcile
        // against the persisted signature, never written Failed here.
        assert!(
            storage_rx.try_recv().is_err(),
            "persisted mint timeout must not write a terminal status",
        );
    }

    /// poll_in_flight with an empty in_flight must be a no-op (no RPC call, no storage update).
    #[tokio::test]
    async fn poll_in_flight_empty_is_noop() {
        let mut state = make_sender_state();
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        // No mock server needed — should not make any RPC call.
        poll_in_flight(&mut state, &storage_tx).await;

        assert!(state.in_flight.is_empty());
        assert!(storage_rx.try_recv().is_err());
    }

    /// A mixed batch (one confirmed, one pending) must resolve the confirmed entry while
    /// keeping the pending entry in in_flight with an incremented poll_attempts.
    #[tokio::test]
    async fn poll_in_flight_mixed_batch_partial_resolution() {
        let mut server = mockito::Server::new_async().await;

        let sig1 = Signature::new_unique();
        let sig2 = Signature::new_unique();

        let _m = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "getSignatureStatuses"
            })))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "context": {"slot": 200},
                        "value": [
                            // sig1 confirmed
                            {
                                "confirmationStatus": "finalized",
                                "confirmations": null,
                                "err": null,
                                "slot": 200,
                                "status": {"Ok": null}
                            },
                            // sig2 not yet confirmed
                            null
                        ]
                    }
                })
                .to_string(),
            )
            .create();

        let storage = Arc::new(Storage::Mock(MockStorage::new()));
        let mut state = SenderState {
            rpc_client: Arc::new(RpcClientWithRetry::with_retry_config(
                server.url(),
                crate::operator::utils::rpc_util::RetryConfig {
                    max_attempts: 1,
                    base_delay: std::time::Duration::from_millis(1),
                    max_delay: std::time::Duration::from_millis(1),
                },
                CommitmentConfig::confirmed(),
            )),
            source_rpc_client: Arc::new(RpcClientWithRetry::with_retry_config(
                server.url(),
                crate::operator::utils::rpc_util::RetryConfig {
                    max_attempts: 1,
                    base_delay: std::time::Duration::from_millis(1),
                    max_delay: std::time::Duration::from_millis(1),
                },
                CommitmentConfig::confirmed(),
            )),
            fallback_rpc_client: None,
            storage: storage.clone(),
            instance_pda: None,
            smt_state: None,
            retry_counts: HashMap::new(),
            mint_builders: HashMap::new(),
            mint_cache: crate::operator::MintCache::new(storage),
            retry_max_attempts: 3,
            confirmation_poll_interval_ms: 400,
            rotation_retry_queue: Vec::new(),
            ambiguous_retry_queue: Vec::new(),
            pending_rotation: None,
            program_type: ProgramType::Escrow,
            remint_cache: HashMap::new(),
            pending_signatures: HashMap::new(),
            pending_remints: Vec::new(),
            in_flight: {
                let q = InFlightQueue::new();
                q.push(make_in_flight_tx(sig1, 201));
                q.push(make_in_flight_tx(sig2, 202));
                q
            },
            semaphore: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
        };

        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        poll_in_flight(&mut state, &storage_tx).await;

        // sig1 resolved — only sig2 remains.
        assert_eq!(state.in_flight.len(), 1, "only pending tx remains");
        {
            let guard = state.in_flight.entries.lock().unwrap();
            assert_eq!(guard[0].ctx.transaction_id, Some(202));
            assert_eq!(guard[0].poll_attempts, 1);
        }

        // Completed for sig1, nothing for sig2 yet.
        let update = storage_rx.try_recv().expect("expected Completed for sig1");
        assert_eq!(update.transaction_id, 201);
        assert_eq!(update.status, TransactionStatus::Completed);
        assert!(storage_rx.try_recv().is_err(), "no update for pending sig2");
    }

    // ── poll_in_flight: chunking ──────────────────────────────────────

    /// When in_flight exceeds 256 entries (the getSignatureStatuses limit), poll_in_flight
    /// must issue multiple RPC calls, one per 256-sig chunk, and merge the results.
    ///
    /// Each chunk response is sized to exactly the number of signatures requested
    /// (256 then 44) so strict length validation passes and the legitimate multi-chunk
    /// merge is exercised. We seed 300 all-null entries and assert the mock was hit at
    /// least twice and that all 300 entries stay in-flight (none confirmed, none dropped).
    #[tokio::test]
    async fn poll_in_flight_chunks_large_batch() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "getSignatureStatuses"
            })))
            .with_status(200)
            .with_body_from_request(|req| {
                let v: serde_json::Value =
                    serde_json::from_slice(req.body().expect("request body present"))
                        .expect("request body is json");
                let requested = v["params"][0].as_array().map(|a| a.len()).unwrap_or(0);
                null_value_body(requested).into_bytes()
            })
            .expect_at_least(2) // 256 sigs -> chunk 1; 44 sigs -> chunk 2
            .create();

        let total = 300usize;
        let mut state = make_sender_state_with_server(&server.url());
        for i in 0..total {
            state
                .in_flight
                .push(make_in_flight_tx(Signature::new_unique(), i as i64 + 1));
        }

        let (storage_tx, _rx) = mpsc::channel(10);
        poll_in_flight(&mut state, &storage_tx).await;

        // All entries stay in-flight (all statuses were null, so not confirmed).
        assert_eq!(
            state.in_flight.len(),
            total,
            "all entries must stay in-flight"
        );
        _m.assert(); // verifies >= 2 RPC calls were made
    }

    // ── fetch_statuses_checked: length-gate + wiring ─────────────────────

    // Build an RpcClientWithRetry that points at a mockito server and fails fast.
    fn make_rpc_client(url: &str) -> RpcClientWithRetry {
        RpcClientWithRetry::with_retry_config(
            url.to_string(),
            RetryConfig {
                max_attempts: 1,
                base_delay: std::time::Duration::from_millis(1),
                max_delay: std::time::Duration::from_millis(1),
            },
            CommitmentConfig::confirmed(),
        )
    }

    // A getSignatureStatuses response body with `count` null status slots.
    fn null_value_body(count: usize) -> String {
        let value = vec![serde_json::Value::Null; count];
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"context": {"slot": 1}, "value": value}
        })
        .to_string()
    }

    // A getSignatureStatuses response body with `count` finalized-success slots.
    fn finalized_value_body(count: usize) -> String {
        let one = serde_json::json!({
            "confirmationStatus": "finalized",
            "confirmations": null,
            "err": null,
            "slot": 100,
            "status": {"Ok": null}
        });
        let value = vec![one; count];
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"context": {"slot": 100}, "value": value}
        })
        .to_string()
    }

    // Mock getSignatureStatuses; the per-call counter lets a test shape one chunk while sizing the rest to their request.
    fn mock_status_bodies<F>(server: &mut mockito::ServerGuard, f: F) -> mockito::Mock
    where
        F: Fn(usize, usize) -> String + Send + Sync + 'static,
    {
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "getSignatureStatuses"
            })))
            .with_status(200)
            .with_body_from_request(move |req| {
                let idx = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let body = req.body().expect("request body present");
                let v: serde_json::Value =
                    serde_json::from_slice(body).expect("request body is json");
                let requested = v["params"][0].as_array().map(|a| a.len()).unwrap_or(0);
                f(idx, requested).into_bytes()
            })
            .expect_at_least(1)
            .create()
    }

    // ── fetch_statuses_checked: response shape matrix ────────────────────

    // An exactly-sized single chunk returns Ok with the requested length.
    #[tokio::test]
    async fn fetch_statuses_e1_exact_single_chunk_ok() {
        let mut server = mockito::Server::new_async().await;
        let _m = mock_status_bodies(&mut server, |_idx, req| null_value_body(req));
        let rpc = make_rpc_client(&server.url());
        let sigs: Vec<Signature> = (0..10).map(|_| Signature::new_unique()).collect();

        let out = fetch_statuses_checked(&rpc, &sigs).await;
        let statuses = out.expect("exact chunk must be Ok");
        assert_eq!(statuses.len(), 10);
    }

    // A short single chunk (N-1 for N) is rejected.
    #[tokio::test]
    async fn fetch_statuses_e2_short_single_chunk_err() {
        let mut server = mockito::Server::new_async().await;
        let _m = mock_status_bodies(&mut server, |_idx, req| null_value_body(req - 1));
        let rpc = make_rpc_client(&server.url());
        let sigs: Vec<Signature> = (0..10).map(|_| Signature::new_unique()).collect();

        assert!(fetch_statuses_checked(&rpc, &sigs).await.is_err());
    }

    // An oversized single chunk (N+1 for N) is rejected.
    #[tokio::test]
    async fn fetch_statuses_e3_oversized_single_chunk_err() {
        let mut server = mockito::Server::new_async().await;
        let _m = mock_status_bodies(&mut server, |_idx, req| null_value_body(req + 1));
        let rpc = make_rpc_client(&server.url());
        let sigs: Vec<Signature> = (0..10).map(|_| Signature::new_unique()).collect();

        assert!(fetch_statuses_checked(&rpc, &sigs).await.is_err());
    }

    // An empty value array for a non-empty request is rejected.
    #[tokio::test]
    async fn fetch_statuses_e4_empty_value_err() {
        let mut server = mockito::Server::new_async().await;
        let _m = mock_status_bodies(&mut server, |_idx, _req| null_value_body(0));
        let rpc = make_rpc_client(&server.url());
        let sigs: Vec<Signature> = (0..5).map(|_| Signature::new_unique()).collect();

        assert!(fetch_statuses_checked(&rpc, &sigs).await.is_err());
    }

    // A multi-chunk request with every chunk exact returns Ok in order; only the first chunk is confirmed.
    #[tokio::test]
    async fn fetch_statuses_e5_multi_chunk_all_exact_ok_ordered() {
        let mut server = mockito::Server::new_async().await;
        let _m = mock_status_bodies(&mut server, |idx, req| {
            if idx == 0 {
                finalized_value_body(req)
            } else {
                null_value_body(req)
            }
        });
        let rpc = make_rpc_client(&server.url());
        // 600 sigs -> chunks of 256 + 256 + 88.
        let sigs: Vec<Signature> = (0..600).map(|_| Signature::new_unique()).collect();

        let statuses = fetch_statuses_checked(&rpc, &sigs)
            .await
            .expect("all-exact multi-chunk must be Ok");
        assert_eq!(statuses.len(), 600);
        // First chunk finalized, remaining chunks null: proves concatenation order.
        assert!(statuses[0].is_some());
        assert!(statuses[255].is_some());
        assert!(statuses[256].is_none());
        assert!(statuses[599].is_none());
    }

    // A short first chunk is rejected.
    #[tokio::test]
    async fn fetch_statuses_e6_short_first_chunk_err() {
        let mut server = mockito::Server::new_async().await;
        let _m = mock_status_bodies(&mut server, |idx, req| {
            if idx == 0 {
                null_value_body(req - 1)
            } else {
                null_value_body(req)
            }
        });
        let rpc = make_rpc_client(&server.url());
        let sigs: Vec<Signature> = (0..600).map(|_| Signature::new_unique()).collect();

        assert!(fetch_statuses_checked(&rpc, &sigs).await.is_err());
    }

    // A short middle chunk is rejected.
    #[tokio::test]
    async fn fetch_statuses_e7_short_middle_chunk_err() {
        let mut server = mockito::Server::new_async().await;
        let _m = mock_status_bodies(&mut server, |idx, req| {
            if idx == 1 {
                null_value_body(req - 1)
            } else {
                null_value_body(req)
            }
        });
        let rpc = make_rpc_client(&server.url());
        let sigs: Vec<Signature> = (0..600).map(|_| Signature::new_unique()).collect();

        assert!(fetch_statuses_checked(&rpc, &sigs).await.is_err());
    }

    // A short final chunk is rejected.
    #[tokio::test]
    async fn fetch_statuses_e8_short_final_chunk_err() {
        let mut server = mockito::Server::new_async().await;
        let _m = mock_status_bodies(&mut server, |idx, req| {
            if idx == 2 {
                null_value_body(req - 1)
            } else {
                null_value_body(req)
            }
        });
        let rpc = make_rpc_client(&server.url());
        let sigs: Vec<Signature> = (0..600).map(|_| Signature::new_unique()).collect();

        assert!(fetch_statuses_checked(&rpc, &sigs).await.is_err());
    }

    // An oversized middle chunk is rejected.
    #[tokio::test]
    async fn fetch_statuses_e9_oversized_middle_chunk_err() {
        let mut server = mockito::Server::new_async().await;
        let _m = mock_status_bodies(&mut server, |idx, req| {
            if idx == 1 {
                null_value_body(req + 1)
            } else {
                null_value_body(req)
            }
        });
        let rpc = make_rpc_client(&server.url());
        let sigs: Vec<Signature> = (0..600).map(|_| Signature::new_unique()).collect();

        assert!(fetch_statuses_checked(&rpc, &sigs).await.is_err());
    }

    // An RPC transport error on a chunk is surfaced as Err (existing behavior).
    #[tokio::test]
    async fn fetch_statuses_e10_rpc_error_err() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "getSignatureStatuses"
            })))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "error": {"code": -32600, "message": "Internal error"}
                })
                .to_string(),
            )
            .create();
        let rpc = make_rpc_client(&server.url());
        let sigs: Vec<Signature> = (0..3).map(|_| Signature::new_unique()).collect();

        assert!(fetch_statuses_checked(&rpc, &sigs).await.is_err());
    }

    // An empty signature slice returns Ok(empty) and issues no RPC call.
    #[tokio::test]
    async fn fetch_statuses_e11_empty_slice_ok_no_call() {
        let mut server = mockito::Server::new_async().await;
        // Any call would be a bug: assert the mock is never hit.
        let m = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "getSignatureStatuses"
            })))
            .with_status(200)
            .with_body(null_value_body(0))
            .expect(0)
            .create();
        let rpc = make_rpc_client(&server.url());

        let statuses = fetch_statuses_checked(&rpc, &[])
            .await
            .expect("empty is Ok");
        assert!(statuses.is_empty());
        m.assert();
    }

    // ── poll_in_flight: wiring and anti-misattribution ───────────────────

    // A short only-chunk reinserts the full batch and settles nothing.
    #[tokio::test]
    async fn poll_in_flight_short_chunk_full_reinsert_no_settlement() {
        let mut server = mockito::Server::new_async().await;
        // Return one fewer status than requested for the single chunk.
        let _m = mock_status_bodies(&mut server, |_idx, req| null_value_body(req - 1));

        let mut state = make_sender_state_with_server(&server.url());
        let ids: Vec<i64> = (1..=5).collect();
        for id in &ids {
            state
                .in_flight
                .push(make_in_flight_tx(Signature::new_unique(), *id));
        }

        let (storage_tx, mut storage_rx) = mpsc::channel(10);
        poll_in_flight(&mut state, &storage_tx).await;

        assert_eq!(state.in_flight.len(), 5, "all entries reinserted");
        {
            let guard = state.in_flight.entries.lock().unwrap();
            let mut present: Vec<i64> = guard.iter().filter_map(|t| t.ctx.transaction_id).collect();
            present.sort_unstable();
            assert_eq!(present, ids, "no entry dropped");
            assert!(
                guard.iter().all(|t| t.poll_attempts == 0),
                "poll_attempts not incremented on a malformed cycle"
            );
        }
        assert!(storage_rx.try_recv().is_err(), "no Completed emitted");
    }

    // Reported repro: 257 entries across a chunk boundary, chunk 1 confirmed and chunk 2 short, must settle nothing and drop nothing.
    #[tokio::test]
    async fn poll_in_flight_cross_chunk_short_no_misattribution() {
        let mut server = mockito::Server::new_async().await;
        // Chunk 0 (256 sigs) all finalized; chunk 1 (1 sig) returns an empty value.
        let _m = mock_status_bodies(&mut server, |idx, req| {
            if idx == 0 {
                finalized_value_body(req)
            } else {
                null_value_body(0)
            }
        });

        let mut state = make_sender_state_with_server(&server.url());
        let ids: Vec<i64> = (1..=257).collect();
        for id in &ids {
            state
                .in_flight
                .push(make_in_flight_tx(Signature::new_unique(), *id));
        }

        let (storage_tx, mut storage_rx) = mpsc::channel(300);
        poll_in_flight(&mut state, &storage_tx).await;

        assert_eq!(state.in_flight.len(), 257, "all 257 entries reinserted");
        {
            let guard = state.in_flight.entries.lock().unwrap();
            let mut present: Vec<i64> = guard.iter().filter_map(|t| t.ctx.transaction_id).collect();
            present.sort_unstable();
            assert_eq!(present, ids, "tail entry (id 257) not dropped");
        }
        assert!(
            storage_rx.try_recv().is_err(),
            "no Completed for any transaction on a malformed cross-chunk cycle"
        );
    }

    // An oversized middle chunk is caught by the same gate as the short-chunk cases.
    #[tokio::test]
    async fn poll_in_flight_oversized_chunk_full_reinsert_no_settlement() {
        let mut server = mockito::Server::new_async().await;
        // Chunk 0 (256) finalized; chunk 1 (1 sig) returns two statuses (oversized).
        let _m = mock_status_bodies(&mut server, |idx, req| {
            if idx == 0 {
                finalized_value_body(req)
            } else {
                null_value_body(req + 1)
            }
        });

        let mut state = make_sender_state_with_server(&server.url());
        let ids: Vec<i64> = (1..=257).collect();
        for id in &ids {
            state
                .in_flight
                .push(make_in_flight_tx(Signature::new_unique(), *id));
        }

        let (storage_tx, mut storage_rx) = mpsc::channel(300);
        poll_in_flight(&mut state, &storage_tx).await;

        assert_eq!(state.in_flight.len(), 257, "all entries reinserted");
        assert!(storage_rx.try_recv().is_err(), "no Completed emitted");
    }

    // Happy-path multi-chunk: every chunk returns correctly-sized finalized statuses, so all entries settle with one Completed each.
    #[tokio::test]
    async fn poll_in_flight_multi_chunk_confirmed_settles_with_correct_pairing() {
        let mut server = mockito::Server::new_async().await;
        let _m = mock_status_bodies(&mut server, |_idx, req| finalized_value_body(req));

        let mut state = make_sender_state_with_server(&server.url());
        let total = 300usize;
        // Map each transaction_id to the signature we seeded it with.
        let mut sig_by_id: std::collections::HashMap<i64, String> = HashMap::new();
        for i in 0..total {
            let sig = Signature::new_unique();
            let id = i as i64 + 1;
            sig_by_id.insert(id, sig.to_string());
            state.in_flight.push(make_in_flight_tx(sig, id));
        }

        let (storage_tx, mut storage_rx) = mpsc::channel(total + 10);
        poll_in_flight(&mut state, &storage_tx).await;

        assert!(state.in_flight.is_empty(), "all confirmed entries settled");
        let mut seen = 0usize;
        while let Ok(update) = storage_rx.try_recv() {
            assert_eq!(update.status, TransactionStatus::Completed);
            // Each Completed must carry the requesting transaction's own signature.
            assert_eq!(
                update.counterpart_signature.as_deref(),
                sig_by_id.get(&update.transaction_id).map(|s| s.as_str()),
                "Completed must pair each transaction with its own signature"
            );
            seen += 1;
        }
        assert_eq!(seen, total, "exactly one Completed per transaction");
    }

    // ── run_poll_task: parity with poll_in_flight ────────────────────────

    // A short chunk on the production poll task reinserts the batch and settles nothing (no PollTaskResult, no Completed).
    #[tokio::test]
    async fn run_poll_task_short_chunk_reinserts_no_settlement() {
        let mut server = mockito::Server::new_async().await;
        let _m = mock_status_bodies(&mut server, |_idx, req| null_value_body(req - 1));

        let in_flight = InFlightQueue::new();
        let (result_tx, mut result_rx) = mpsc::channel::<Vec<PollTaskResult>>(8);
        let (storage_tx, mut storage_rx) = mpsc::channel(10);
        let rpc = Arc::new(make_rpc_client(&server.url()));
        let token = tokio_util::sync::CancellationToken::new();

        let ids: Vec<i64> = (1..=5).collect();
        for id in &ids {
            in_flight.push(make_in_flight_tx(Signature::new_unique(), *id));
        }

        let handle = tokio::spawn(run_poll_task(
            in_flight.clone(),
            result_tx,
            rpc,
            storage_tx,
            ProgramType::Escrow,
            5,
            token.clone(),
        ));

        // Give the task time to drain, poll, hit the short response, and reinsert.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        token.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("task must exit after cancellation")
            .expect("task must not panic");

        assert!(
            result_rx.try_recv().is_err(),
            "no PollTaskResult on malformed cycle"
        );
        assert!(
            storage_rx.try_recv().is_err(),
            "no Completed on malformed cycle"
        );
        assert_eq!(in_flight.len(), 5, "batch reinserted after short chunk");
        let mut present: Vec<i64> = {
            let guard = in_flight.entries.lock().unwrap();
            guard.iter().filter_map(|t| t.ctx.transaction_id).collect()
        };
        present.sort_unstable();
        assert_eq!(present, ids, "no entry dropped");
        // Prove the task actually polled, so the negative assertions above are not vacuous.
        _m.assert();
    }

    // Happy-path finalized multi-chunk on the production task settles every entry and emits a Completed for each.
    #[tokio::test]
    async fn run_poll_task_multi_chunk_confirmed_settles() {
        let mut server = mockito::Server::new_async().await;
        let _m = mock_status_bodies(&mut server, |_idx, req| finalized_value_body(req));

        let in_flight = InFlightQueue::new();
        let (result_tx, _result_rx) = mpsc::channel::<Vec<PollTaskResult>>(8);
        let (storage_tx, mut storage_rx) = mpsc::channel(400);
        let rpc = Arc::new(make_rpc_client(&server.url()));
        let token = tokio_util::sync::CancellationToken::new();

        let total = 300usize;
        let mut sig_by_id: std::collections::HashMap<i64, String> = HashMap::new();
        for i in 0..total {
            let sig = Signature::new_unique();
            let id = i as i64 + 1;
            sig_by_id.insert(id, sig.to_string());
            in_flight.push(make_in_flight_tx(sig, id));
        }

        let handle = tokio::spawn(run_poll_task(
            in_flight.clone(),
            result_tx,
            rpc,
            storage_tx,
            ProgramType::Escrow,
            5,
            token.clone(),
        ));

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        token.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("task must exit after cancellation")
            .expect("task must not panic");

        assert_eq!(in_flight.len(), 0, "all confirmed entries settled");
        let mut seen = 0usize;
        while let Ok(update) = storage_rx.try_recv() {
            assert_eq!(update.status, TransactionStatus::Completed);
            assert_eq!(
                update.counterpart_signature.as_deref(),
                sig_by_id.get(&update.transaction_id).map(|s| s.as_str()),
                "Completed must pair each transaction with its own signature"
            );
            seen += 1;
        }
        assert_eq!(seen, total, "exactly one Completed per transaction");
    }

    /// An idempotent tx that exhausts its resend_count budget must be declared a
    /// permanent failure rather than re-queued indefinitely (infinite loop guard).
    #[tokio::test]
    async fn poll_in_flight_idempotent_resend_limit_triggers_permanent_failure() {
        let mut server = mockito::Server::new_async().await;

        let sig = Signature::new_unique();

        // RPC returns null (not confirmed) — triggering the timeout arm.
        let _m = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "getSignatureStatuses"
            })))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "context": {"slot": 10},
                        "value": [null]
                    }
                })
                .to_string(),
            )
            .expect_at_least(1)
            .create();

        let retry_max = 2u32;
        let mut state = make_sender_state_with_server(&server.url());
        state.retry_max_attempts = retry_max;
        {
            let mut tx = make_in_flight_tx(sig, 77);
            tx.retry_policy = RetryPolicy::Idempotent;
            // Already at the cap — next_resend (3) > retry_max (2).
            tx.resend_count = retry_max;
            tx.poll_attempts = MAX_POLL_ATTEMPTS_CONFIRMATION; // trigger timeout arm
            *state.in_flight.entries.lock().unwrap() = vec![tx];
        }

        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        poll_in_flight(&mut state, &storage_tx).await;

        // Must have been removed from in_flight.
        assert!(
            state.in_flight.is_empty(),
            "exhausted tx must leave in_flight"
        );

        // Permanent failure status must be emitted.
        let update = storage_rx
            .try_recv()
            .expect("expected permanent-failure status update");
        assert_eq!(update.transaction_id, 77);
        assert_eq!(update.status, TransactionStatus::Failed);
        assert!(
            update
                .error_message
                .as_deref()
                .unwrap_or("")
                .contains("resend limit"),
            "error message should mention resend limit: {:?}",
            update.error_message
        );
    }

    // ── fire_and_store_task: deposit-mint pre-broadcast persist ───────

    fn mint_ctx(txn_id: i64) -> TransactionContext {
        TransactionContext {
            transaction_id: Some(txn_id),
            withdrawal_nonce: None,
            trace_id: Some(format!("trace-{txn_id}")),
            deposit_claim_lease: None,
        }
    }

    fn mint_ctx_with_lease(
        txn_id: i64,
        lease: chrono::DateTime<chrono::Utc>,
    ) -> TransactionContext {
        TransactionContext {
            deposit_claim_lease: Some(lease),
            ..mint_ctx(txn_id)
        }
    }

    fn recoverable(lease: chrono::DateTime<chrono::Utc>) -> SendDurability {
        SendDurability::Recoverable {
            deposit_expected_updated_at: lease,
        }
    }

    /// A failed write-ahead persist on the mint path must NOT broadcast, must stash no
    /// in-flight entry, and must write no terminal status (row left Processing).
    #[tokio::test]
    async fn mint_persist_failure_aborts_before_broadcast() {
        let mut server = mockito::Server::new_async().await;
        let _hash = mock_blockhash(&mut server);
        let send = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "sendTransaction"
            })))
            .expect(0)
            .create();

        let state = make_sender_state_with_server(&server.url());
        let Storage::Mock(ref mock) = *state.storage else {
            panic!("expected mock storage");
        };
        let t_lock = Utc::now();
        seed_mock_deposit(&state, 77, TransactionStatus::Processing, t_lock);
        mock.set_should_fail("insert_release_signature", true);
        let permit = state.semaphore.clone().try_acquire_owned().unwrap();
        let before = state.semaphore.available_permits();
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        fire_and_store_task(
            state.rpc_client.clone(),
            state.storage.clone(),
            state.in_flight.clone(),
            state.program_type,
            dummy_instruction(),
            None,
            mint_ctx(77),
            RetryPolicy::None,
            ExtraErrorCheckPolicy::None,
            storage_tx,
            recoverable(t_lock),
            permit,
        )
        .await;

        send.assert();
        assert!(
            storage_rx.try_recv().is_err(),
            "no status update; row stays Processing for recovery"
        );
        assert!(
            state.in_flight.is_empty(),
            "nothing stashed when persist failed"
        );
        assert_eq!(
            state.semaphore.available_permits(),
            before + 1,
            "permit must be dropped on abort"
        );
    }

    /// A send error after the signature is persisted may still have landed on-chain, so the
    /// mint is never terminalized in the sender: the signature is kept and no status update is
    /// written (row left Processing for recovery to reconcile against the persisted signature).
    #[tokio::test]
    async fn mint_send_error_after_persist_left_for_recovery() {
        let mut server = mockito::Server::new_async().await;
        let _hash = mock_blockhash(&mut server);
        let _send = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "sendTransaction"
            })))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "error": {"code": -32600, "message": "Internal error"}
                })
                .to_string(),
            )
            .create();

        let state = make_sender_state_with_server(&server.url());
        let t_lock = Utc::now();
        seed_mock_deposit(&state, 77, TransactionStatus::Processing, t_lock);
        let permit = state.semaphore.clone().try_acquire_owned().unwrap();
        let before = state.semaphore.available_permits();
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        fire_and_store_task(
            state.rpc_client.clone(),
            state.storage.clone(),
            state.in_flight.clone(),
            state.program_type,
            dummy_instruction(),
            None,
            mint_ctx(77),
            RetryPolicy::None,
            ExtraErrorCheckPolicy::None,
            storage_tx,
            recoverable(t_lock),
            permit,
        )
        .await;

        let Storage::Mock(ref mock) = *state.storage else {
            panic!("expected mock storage");
        };
        assert!(
            !mock.get_release_signatures(77).await.unwrap().is_empty(),
            "signature must be preserved so recovery can reconcile it",
        );
        assert!(
            storage_rx.try_recv().is_err(),
            "no status update; row left Processing for recovery",
        );
        assert!(
            state.in_flight.is_empty(),
            "a failed broadcast stashes no in-flight entry",
        );
        assert_eq!(
            state.semaphore.available_permits(),
            before + 1,
            "permit must be dropped on send error",
        );
    }

    /// A preflight-style RPC rejection after the signature is persisted is deferred to
    /// recovery too: even a "deterministic" program error can be a stale-node false negative,
    /// so the sender never marks a persisted mint Failed and keeps the signature.
    #[tokio::test]
    async fn mint_preflight_failure_after_persist_left_for_recovery() {
        let mut server = mockito::Server::new_async().await;
        let _hash = mock_blockhash(&mut server);
        // A preflight failure surfaces as a distinct RPC response error code (-32002); the
        // sender's branch treats every persisted send error identically, so recovery decides.
        let _send = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "sendTransaction"
            })))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "error": {
                        "code": -32002,
                        "message": "Transaction simulation failed",
                        "data": {
                            "err": {"InstructionError": [0, {"Custom": 1}]},
                            "logs": ["Program log: preflight failure"]
                        }
                    }
                })
                .to_string(),
            )
            .create();

        let state = make_sender_state_with_server(&server.url());
        let t_lock = Utc::now();
        seed_mock_deposit(&state, 77, TransactionStatus::Processing, t_lock);
        let permit = state.semaphore.clone().try_acquire_owned().unwrap();
        let before = state.semaphore.available_permits();
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        fire_and_store_task(
            state.rpc_client.clone(),
            state.storage.clone(),
            state.in_flight.clone(),
            state.program_type,
            dummy_instruction(),
            None,
            mint_ctx(77),
            RetryPolicy::None,
            ExtraErrorCheckPolicy::None,
            storage_tx,
            recoverable(t_lock),
            permit,
        )
        .await;

        let Storage::Mock(ref mock) = *state.storage else {
            panic!("expected mock storage");
        };
        assert!(
            !mock.get_release_signatures(77).await.unwrap().is_empty(),
            "signature must be preserved so recovery can reconcile it",
        );
        assert!(
            storage_rx.try_recv().is_err(),
            "a preflight rejection writes no status; row left Processing for recovery",
        );
        assert!(
            state.in_flight.is_empty(),
            "a failed broadcast stashes no in-flight entry",
        );
        assert_eq!(
            state.semaphore.available_permits(),
            before + 1,
            "permit must be dropped on send error",
        );
    }

    /// A Terminal (non-persisted) send error still fails fast: InitializeMint mints no
    /// balance, so there is nothing to strand and the row is terminalized to Failed.
    #[tokio::test]
    async fn terminal_send_error_routes_to_failed() {
        let mut server = mockito::Server::new_async().await;
        let _hash = mock_blockhash(&mut server);
        let _send = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "sendTransaction"
            })))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "error": {"code": -32600, "message": "Internal error"}
                })
                .to_string(),
            )
            .create();

        let state = make_sender_state_with_server(&server.url());
        let permit = state.semaphore.clone().try_acquire_owned().unwrap();
        let before = state.semaphore.available_permits();
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        fire_and_store_task(
            state.rpc_client.clone(),
            state.storage.clone(),
            state.in_flight.clone(),
            state.program_type,
            dummy_instruction(),
            None,
            mint_ctx(909),
            RetryPolicy::Idempotent,
            ExtraErrorCheckPolicy::None,
            storage_tx,
            SendDurability::Terminal,
            permit,
        )
        .await;

        let update = storage_rx
            .try_recv()
            .expect("a terminal send error must emit a status update");
        assert_eq!(update.transaction_id, 909);
        assert_eq!(update.status, TransactionStatus::Failed);
        assert!(
            state.in_flight.is_empty(),
            "a failed broadcast stashes no in-flight entry",
        );
        assert_eq!(
            state.semaphore.available_permits(),
            before + 1,
            "permit must be dropped on send error",
        );
    }

    /// A build/sign failure on a Recoverable mint happens before any signature exists or is
    /// broadcast, so it must not be terminalized: no status update is written (row left
    /// Processing for the recovery sweep to re-mint) and the permit is released.
    #[tokio::test]
    async fn mint_build_sign_failure_leaves_processing() {
        let txn_id = 77;
        let mut server = mockito::Server::new_async().await;
        // getLatestBlockhash fails, so build_and_sign returns Err before signing or sending.
        let _hash = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "getLatestBlockhash"
            })))
            .with_status(500)
            .with_body("blockhash rpc down")
            .create();
        let send = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "sendTransaction"
            })))
            .expect(0)
            .create();

        let state = make_sender_state_with_server(&server.url());
        let permit = state.semaphore.clone().try_acquire_owned().unwrap();
        let before = state.semaphore.available_permits();
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        fire_and_store_task(
            state.rpc_client.clone(),
            state.storage.clone(),
            state.in_flight.clone(),
            state.program_type,
            dummy_instruction(),
            None,
            mint_ctx(txn_id),
            RetryPolicy::None,
            ExtraErrorCheckPolicy::None,
            storage_tx,
            recoverable(Utc::now()),
            permit,
        )
        .await;

        send.assert();
        assert!(
            storage_rx.try_recv().is_err(),
            "build/sign failure must emit no terminal status; row left Processing for recovery",
        );
        assert!(
            state.in_flight.is_empty(),
            "nothing stashed when build/sign failed",
        );
        let Storage::Mock(ref mock) = *state.storage else {
            panic!("expected mock storage");
        };
        assert!(
            mock.get_release_signatures(txn_id)
                .await
                .unwrap()
                .is_empty(),
            "no signature persisted when build/sign failed before signing",
        );
        assert_eq!(
            state.semaphore.available_permits(),
            before + 1,
            "permit must be released on build/sign failure",
        );
    }

    // ── JIT mint retry: write-ahead persist before broadcast ──────

    static INIT_TEST_SIGNER: std::sync::Once = std::sync::Once::new();

    /// Configure an in-memory admin signer so `SignerUtil::admin_signer()` can
    /// resolve inside the JIT pre-check. Must run before the first access to the
    /// process-global signer Lazy, so every test touching it calls this first.
    fn ensure_test_signer() {
        INIT_TEST_SIGNER.call_once(|| {
            let kp = solana_sdk::signer::keypair::Keypair::new();
            let b58 = bs58::encode(kp.to_bytes()).into_string();
            std::env::set_var("ADMIN_SIGNER", "memory");
            std::env::set_var("ADMIN_PRIVATE_KEY", &b58);
        });
    }

    /// Mock `getAccountInfo` returning a packed, initialized SPL `Mint` whose
    /// `mint_authority` equals `authority`, owned by the SPL token program. This
    /// makes the JIT pre-check decode an `AuthorityCheck::Match`, so
    /// `try_jit_mint_initialization` returns `JitOutcome::Retry` without sending
    /// an on-chain InitializeMint. Matched by method only, so a single mock
    /// answers any mint pubkey the pre-check looks up.
    fn mock_get_account_info_mint(
        server: &mut mockito::ServerGuard,
        authority: Pubkey,
    ) -> mockito::Mock {
        use spl_token::solana_program::program_option::COption;
        use spl_token::solana_program::program_pack::Pack;
        use spl_token::state::Mint;

        let mint = Mint {
            mint_authority: COption::Some(authority),
            supply: 0,
            decimals: 6,
            is_initialized: true,
            freeze_authority: COption::None,
        };
        let mut data = vec![0u8; Mint::LEN];
        Mint::pack(mint, &mut data).expect("pack mint");
        server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "getAccountInfo"
            })))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "context": {"slot": 1},
                        "value": {
                            "owner": spl_token::id().to_string(),
                            "lamports": 1_000_000u64,
                            "data": [STANDARD.encode(&data), "base64"],
                            "executable": false,
                            "rentEpoch": 0
                        }
                    }
                })
                .to_string(),
            )
            .create()
    }

    /// Seed `mint_builders[txn_id]` with a builder carrying `mint` so the JIT
    /// pre-check's `builder.get_mint()` is `Some`.
    fn seed_mint_builder(state: &mut SenderState, txn_id: i64, mint: Pubkey) {
        use crate::operator::utils::instruction_util::MintToBuilder;
        let mut builder = MintToBuilder::new();
        builder.mint(mint);
        state.mint_builders.insert(txn_id, builder);
    }

    /// A JIT `Retry` verdict must journal the value-bearing retry signature
    /// (write-ahead) before broadcasting it and stash a persisted in-flight tx.
    #[tokio::test]
    async fn jit_retry_persists_signature_before_broadcast() {
        use crate::operator::SignerUtil;

        ensure_test_signer();
        let mut server = mockito::Server::new_async().await;
        let admin = SignerUtil::admin_signer().pubkey();
        let _acct = mock_get_account_info_mint(&mut server, admin);
        let _hash = mock_blockhash(&mut server);
        let send = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "sendTransaction"
            })))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": Signature::default().to_string()
                })
                .to_string(),
            )
            .expect(1)
            .create();

        let mut state = make_sender_state_with_server(&server.url());
        seed_mint_builder(&mut state, 77, Pubkey::new_unique());
        let t_lock = Utc::now();
        seed_mock_deposit(&state, 77, TransactionStatus::Processing, t_lock);
        let ctx = mint_ctx_with_lease(77, t_lock);
        let (storage_tx, _rx) = mpsc::channel(10);

        handle_confirmation_result(
            &mut state,
            Ok(ConfirmationResult::MintNotInitialized),
            Signature::new_unique(),
            None,
            &ctx,
            dummy_instruction(),
            RetryPolicy::None,
            &ExtraErrorCheckPolicy::None,
            &storage_tx,
        )
        .await;

        send.assert();
        let Storage::Mock(ref mock) = *state.storage else {
            panic!("expected mock storage");
        };
        let stored = mock.get_release_signatures(77).await.unwrap();
        assert_eq!(stored.len(), 1, "exactly one retry signature journaled");
        assert_eq!(
            stored[0].0,
            Signature::default().to_string(),
            "journaled signature must be the broadcast signature"
        );
        assert_eq!(stored[0].1, 100, "journaled lvbh must match the blockhash");
        assert_eq!(
            state.in_flight.len(),
            1,
            "successful broadcast stashes the in-flight tx"
        );
        assert!(
            state.in_flight.entries.lock().unwrap()[0].persisted,
            "the stashed JIT-retry tx must be marked persisted"
        );
        let stashed_lease = state.in_flight.entries.lock().unwrap()[0]
            .ctx
            .deposit_claim_lease;
        assert!(
            stashed_lease.is_some_and(|lease| lease != t_lock),
            "the stashed JIT-retry ctx must carry the advanced claim lease"
        );
    }

    /// A failed write-ahead persist on the JIT retry must abort before
    /// broadcast, leave the row Processing (no status update), stash nothing, and
    /// release the permit.
    #[tokio::test]
    async fn jit_retry_persist_failure_aborts_before_broadcast() {
        use crate::operator::SignerUtil;

        ensure_test_signer();
        let mut server = mockito::Server::new_async().await;
        let admin = SignerUtil::admin_signer().pubkey();
        let _acct = mock_get_account_info_mint(&mut server, admin);
        let _hash = mock_blockhash(&mut server);
        let send = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "sendTransaction"
            })))
            .expect(0)
            .create();

        let mut state = make_sender_state_with_server(&server.url());
        seed_mint_builder(&mut state, 77, Pubkey::new_unique());
        let Storage::Mock(ref mock) = *state.storage else {
            panic!("expected mock storage");
        };
        let t_lock = Utc::now();
        seed_mock_deposit(&state, 77, TransactionStatus::Processing, t_lock);
        mock.set_should_fail("claim_and_persist_deposit_signature", true);
        let before = state.semaphore.available_permits();
        let ctx = mint_ctx_with_lease(77, t_lock);
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        handle_confirmation_result(
            &mut state,
            Ok(ConfirmationResult::MintNotInitialized),
            Signature::new_unique(),
            None,
            &ctx,
            dummy_instruction(),
            RetryPolicy::None,
            &ExtraErrorCheckPolicy::None,
            &storage_tx,
        )
        .await;

        send.assert();
        assert!(
            storage_rx.try_recv().is_err(),
            "no status update; row stays Processing for recovery"
        );
        assert!(
            state.in_flight.is_empty(),
            "nothing stashed when persist failed"
        );
        assert_eq!(
            state.semaphore.available_permits(),
            before,
            "permit must be released on abort"
        );
    }

    /// A JIT retry with no carried ownership lease must fail closed: no
    /// signature, no broadcast, no status update, and the row remains for
    /// recovery. This is defensive; normal deposit mints get the lease from the
    /// first successful claim.
    #[tokio::test]
    async fn jit_refire_missing_lease_fails_closed() {
        use crate::operator::SignerUtil;

        ensure_test_signer();
        let mut server = mockito::Server::new_async().await;
        let admin = SignerUtil::admin_signer().pubkey();
        let _acct = mock_get_account_info_mint(&mut server, admin);
        let send = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "sendTransaction"
            })))
            .expect(0)
            .create();

        let mut state = make_sender_state_with_server(&server.url());
        seed_mint_builder(&mut state, 77, Pubkey::new_unique());
        let metric = metrics::OPERATOR_TRANSACTION_ERRORS
            .with_label_values(&[state.program_type.as_label(), "jit_missing_claim_lease"]);
        let before_metric = metric.get();
        let ctx = mint_ctx(77);
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        handle_confirmation_result(
            &mut state,
            Ok(ConfirmationResult::MintNotInitialized),
            Signature::new_unique(),
            None,
            &ctx,
            dummy_instruction(),
            RetryPolicy::None,
            &ExtraErrorCheckPolicy::None,
            &storage_tx,
        )
        .await;

        send.assert();
        let Storage::Mock(ref mock) = *state.storage else {
            panic!("expected mock storage");
        };
        assert!(
            mock.get_release_signatures(77).await.unwrap().is_empty(),
            "missing lease path must journal nothing"
        );
        assert!(
            storage_rx.try_recv().is_err(),
            "missing lease path must leave the row Processing"
        );
        assert!(state.in_flight.is_empty(), "missing lease stashes nothing");
        assert_eq!(
            metric.get(),
            before_metric + 1.0,
            "missing lease increments jit_missing_claim_lease"
        );
    }

    /// A stale JIT retry lease means recovery or a new incarnation owns the row.
    /// The re-fire must drop without journaling or broadcasting.
    #[tokio::test]
    async fn jit_refire_aborts_when_ownership_lost() {
        use crate::operator::SignerUtil;

        ensure_test_signer();
        let mut server = mockito::Server::new_async().await;
        let admin = SignerUtil::admin_signer().pubkey();
        let _acct = mock_get_account_info_mint(&mut server, admin);
        let _hash = mock_blockhash(&mut server);
        let send = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "sendTransaction"
            })))
            .expect(0)
            .create();

        let mut state = make_sender_state_with_server(&server.url());
        seed_mint_builder(&mut state, 77, Pubkey::new_unique());
        let t_lock = Utc::now();
        seed_mock_deposit(&state, 77, TransactionStatus::Pending, t_lock);
        let metric = metrics::OPERATOR_TRANSACTION_ERRORS
            .with_label_values(&[state.program_type.as_label(), "deposit_ownership_lost"]);
        let before_metric = metric.get();
        let before_permits = state.semaphore.available_permits();
        let ctx = mint_ctx_with_lease(77, t_lock);
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        handle_confirmation_result(
            &mut state,
            Ok(ConfirmationResult::MintNotInitialized),
            Signature::new_unique(),
            None,
            &ctx,
            dummy_instruction(),
            RetryPolicy::None,
            &ExtraErrorCheckPolicy::None,
            &storage_tx,
        )
        .await;

        send.assert();
        let Storage::Mock(ref mock) = *state.storage else {
            panic!("expected mock storage");
        };
        assert!(
            mock.get_release_signatures(77).await.unwrap().is_empty(),
            "lost JIT claim must persist no retry signature"
        );
        assert!(
            storage_rx.try_recv().is_err(),
            "lost JIT claim writes no status"
        );
        assert!(state.in_flight.is_empty(), "lost JIT claim stashes nothing");
        assert_eq!(
            state.semaphore.available_permits(),
            before_permits,
            "the acquired JIT permit must be released on lost claim"
        );
        assert_eq!(
            metric.get(),
            before_metric + 1.0,
            "lost JIT claim increments deposit_ownership_lost"
        );
    }

    /// When the in-flight cap is reached, the JIT retry must not broadcast
    /// (nothing journaled, no status update, nothing stashed) so the row is left
    /// Processing for recovery.
    #[tokio::test]
    async fn jit_retry_cap_reached_leaves_processing() {
        use crate::operator::SignerUtil;

        ensure_test_signer();
        let mut server = mockito::Server::new_async().await;
        let admin = SignerUtil::admin_signer().pubkey();
        let _acct = mock_get_account_info_mint(&mut server, admin);
        let _hash = mock_blockhash(&mut server);
        let send = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "sendTransaction"
            })))
            .expect(0)
            .create();

        let mut state = make_sender_state_with_server(&server.url());
        seed_mint_builder(&mut state, 77, Pubkey::new_unique());
        let t_lock = Utc::now();
        seed_mock_deposit(&state, 77, TransactionStatus::Processing, t_lock);

        // Drain every permit and hold them so the JIT retry can't acquire one.
        let _held: Vec<_> = (0..MAX_IN_FLIGHT)
            .map(|_| state.semaphore.clone().try_acquire_owned().unwrap())
            .collect();
        assert_eq!(state.semaphore.available_permits(), 0);

        let ctx = mint_ctx_with_lease(77, t_lock);
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        handle_confirmation_result(
            &mut state,
            Ok(ConfirmationResult::MintNotInitialized),
            Signature::new_unique(),
            None,
            &ctx,
            dummy_instruction(),
            RetryPolicy::None,
            &ExtraErrorCheckPolicy::None,
            &storage_tx,
        )
        .await;

        send.assert();
        let Storage::Mock(ref mock) = *state.storage else {
            panic!("expected mock storage");
        };
        assert!(
            mock.get_release_signatures(77).await.unwrap().is_empty(),
            "cap path must journal nothing"
        );
        assert!(
            storage_rx.try_recv().is_err(),
            "cap path must leave the row Processing (no status update)"
        );
        assert!(
            state.in_flight.is_empty(),
            "cap path stashes no in-flight entry"
        );
    }

    /// Regression: routed through the real poll path, a MintNotInitialized confirmation
    /// at in-flight saturation must still retry. The confirmed parent releases its slot
    /// before the JIT retry acquires one, so the retry reuses that freed slot instead of
    /// being spuriously refused and deferred to recovery.
    #[tokio::test]
    async fn jit_retry_at_saturation_reuses_freed_parent_slot() {
        use crate::operator::SignerUtil;

        ensure_test_signer();
        let mut server = mockito::Server::new_async().await;
        let admin = SignerUtil::admin_signer().pubkey();

        // Parent mint confirms with an on-chain error the mint policy maps to
        // MintNotInitialized, driving the JIT verdict.
        let sig = Signature::new_unique();
        let _status = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "getSignatureStatuses"
            })))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "context": {"slot": 200},
                        "value": [
                            {
                                "confirmationStatus": "finalized",
                                "confirmations": null,
                                "err": {"InstructionError": [0, "InvalidAccountData"]},
                                "slot": 200,
                                "status": {"Err": {"InstructionError": [0, "InvalidAccountData"]}}
                            }
                        ]
                    }
                })
                .to_string(),
            )
            .create();
        let _acct = mock_get_account_info_mint(&mut server, admin);
        let _hash = mock_blockhash(&mut server);
        let send = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "sendTransaction"
            })))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": Signature::default().to_string()
                })
                .to_string(),
            )
            .expect(1)
            .create();

        let mut state = make_sender_state_with_server(&server.url());
        seed_mint_builder(&mut state, 77, Pubkey::new_unique());
        let t_lock = Utc::now();
        seed_mock_deposit(&state, 77, TransactionStatus::Processing, t_lock);

        // Parent in-flight tx holds a permit drawn from the state semaphore and carries
        // the mint policy so route_poll_results classifies MintNotInitialized.
        let parent = super::super::types::InFlightTx {
            signature: sig,
            ctx: mint_ctx_with_lease(77, t_lock),
            instruction: dummy_instruction(),
            compute_unit_price: None,
            retry_policy: RetryPolicy::None,
            extra_error_checks_policy: mint_extra_error_checks_policy(),
            poll_attempts: 0,
            resend_count: 0,
            persisted: true,
            permit: state.semaphore.clone().try_acquire_owned().unwrap(),
        };
        state.in_flight.push(parent);

        // Hold every other permit so the parent's slot is the only one that can free up.
        let _held: Vec<_> = (0..MAX_IN_FLIGHT - 1)
            .map(|_| state.semaphore.clone().try_acquire_owned().unwrap())
            .collect();
        assert_eq!(state.semaphore.available_permits(), 0);

        let (storage_tx, _rx) = mpsc::channel(10);
        poll_in_flight(&mut state, &storage_tx).await;

        send.assert();
        let Storage::Mock(ref mock) = *state.storage else {
            panic!("expected mock storage");
        };
        assert!(
            !mock.get_release_signatures(77).await.unwrap().is_empty(),
            "JIT retry must reuse the freed parent slot and journal its signature at saturation"
        );
    }

    /// A Terminal run broadcasts without writing any signature even though a
    /// transaction_id is present, proving `durability` (not the id-presence guard) is what
    /// excludes the on-chain-idempotent initialization path from write-ahead persist.
    #[tokio::test]
    async fn initialize_mint_does_not_persist() {
        let mut server = mockito::Server::new_async().await;
        let _hash = mock_blockhash(&mut server);
        let _send = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "sendTransaction"
            })))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": Signature::default().to_string()
                })
                .to_string(),
            )
            .create();

        let state = make_sender_state_with_server(&server.url());
        let permit = state.semaphore.clone().try_acquire_owned().unwrap();
        let (storage_tx, _rx) = mpsc::channel(10);

        // Carry a transaction_id so the assertion exercises `durability` itself
        // rather than the inner id-presence guard short-circuiting.
        let ctx = TransactionContext {
            transaction_id: Some(909),
            withdrawal_nonce: None,
            trace_id: Some("trace-init".to_string()),
            deposit_claim_lease: None,
        };

        fire_and_store_task(
            state.rpc_client.clone(),
            state.storage.clone(),
            state.in_flight.clone(),
            state.program_type,
            dummy_instruction(),
            None,
            ctx,
            RetryPolicy::Idempotent,
            ExtraErrorCheckPolicy::None,
            storage_tx,
            SendDurability::Terminal,
            permit,
        )
        .await;

        let Storage::Mock(ref mock) = *state.storage else {
            panic!("expected mock storage");
        };
        assert!(
            mock.get_release_signatures(909).await.unwrap().is_empty(),
            "Terminal durability must not write a signature even with a transaction_id"
        );
        assert_eq!(
            state.in_flight.len(),
            1,
            "broadcast still stashes in-flight"
        );
    }

    // ── deposit first-fire: ownership-checked claim routing ───────────

    /// Seed one deposit row directly into the mock with an explicit status and
    /// `updated_at` so the claim CAS can be exercised against it.
    fn seed_mock_deposit(
        state: &SenderState,
        id: i64,
        status: TransactionStatus,
        updated_at: chrono::DateTime<chrono::Utc>,
    ) {
        let Storage::Mock(ref mock) = *state.storage else {
            panic!("expected mock storage");
        };
        let row = crate::storage::common::models::DbTransaction {
            id,
            signature: format!("src-sig-{id}"),
            trace_id: format!("trace-{id}"),
            slot: 100,
            initiator: "initiator".to_string(),
            recipient: "recipient".to_string(),
            mint: "mint_addr".to_string(),
            amount: crate::storage::common::amount::TokenAmount(1_000),
            memo: None,
            transaction_type: crate::storage::common::models::TransactionType::Deposit,
            withdrawal_nonce: None,
            status,
            created_at: updated_at,
            updated_at,
            processed_at: None,
            counterpart_signature: None,
            remint_signatures: None,
            remint_last_valid_block_heights: None,
            pending_remint_deadline_at: None,
            finality_check_attempts: 0,
            recovery_requeue_attempts: 0,
            instruction_index: 0,
            inner_index: None,
            landed_remint_signature: None,
        };
        mock.pending_transactions.lock().unwrap().push(row);
    }

    /// A deposit first-fire whose row was demoted (claim `Ok(false)`) must NOT
    /// broadcast, must persist no signature, must write no status, must release
    /// its permit, and must meter `deposit_ownership_lost`. This is the bug
    /// closed in isolation: a stale sender-owned builder cannot double-mint.
    #[tokio::test]
    async fn deposit_first_fire_aborts_when_ownership_lost() {
        let mut server = mockito::Server::new_async().await;
        let _hash = mock_blockhash(&mut server);
        let send = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "sendTransaction"
            })))
            .expect(0)
            .create();

        let state = make_sender_state_with_server(&server.url());
        // Recovery already demoted the row to Pending, so the fetch-time token
        // no longer owns a Processing incarnation.
        let t_lock = Utc::now();
        seed_mock_deposit(&state, 77, TransactionStatus::Pending, t_lock);

        let permit = state.semaphore.clone().try_acquire_owned().unwrap();
        let before_permits = state.semaphore.available_permits();
        let metric = metrics::OPERATOR_TRANSACTION_ERRORS
            .with_label_values(&[state.program_type.as_label(), "deposit_ownership_lost"]);
        let before_metric = metric.get();
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        fire_and_store_task(
            state.rpc_client.clone(),
            state.storage.clone(),
            state.in_flight.clone(),
            state.program_type,
            dummy_instruction(),
            None,
            mint_ctx(77),
            RetryPolicy::None,
            ExtraErrorCheckPolicy::None,
            storage_tx,
            recoverable(t_lock),
            permit,
        )
        .await;

        send.assert();
        let Storage::Mock(ref mock) = *state.storage else {
            panic!("expected mock storage");
        };
        assert!(
            mock.get_release_signatures(77).await.unwrap().is_empty(),
            "a lost claim must persist no signature"
        );
        assert!(
            storage_rx.try_recv().is_err(),
            "a lost claim writes no status; the row's current owner handles it"
        );
        assert!(
            state.in_flight.is_empty(),
            "nothing broadcast, nothing stashed in-flight"
        );
        assert_eq!(
            state.semaphore.available_permits(),
            before_permits + 1,
            "the permit must be released on abort"
        );
        assert_eq!(
            metric.get(),
            before_metric + 1.0,
            "a lost claim increments deposit_ownership_lost"
        );
    }

    /// A deposit first-fire that still owns its Processing incarnation (claim
    /// `Ok(true)`) mints exactly once: the signature is persisted, the tx is
    /// broadcast and stashed in-flight, and the token advances (the D3 bump).
    #[tokio::test]
    async fn deposit_first_fire_broadcasts_when_owned() {
        let mut server = mockito::Server::new_async().await;
        let _hash = mock_blockhash(&mut server);
        let send = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "sendTransaction"
            })))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": Signature::default().to_string()
                })
                .to_string(),
            )
            .expect(1)
            .create();

        let state = make_sender_state_with_server(&server.url());
        let t_lock = Utc::now();
        seed_mock_deposit(&state, 77, TransactionStatus::Processing, t_lock);

        let permit = state.semaphore.clone().try_acquire_owned().unwrap();
        let (storage_tx, _rx) = mpsc::channel(10);

        fire_and_store_task(
            state.rpc_client.clone(),
            state.storage.clone(),
            state.in_flight.clone(),
            state.program_type,
            dummy_instruction(),
            None,
            mint_ctx(77),
            RetryPolicy::None,
            ExtraErrorCheckPolicy::None,
            storage_tx,
            recoverable(t_lock),
            permit,
        )
        .await;

        send.assert();
        let Storage::Mock(ref mock) = *state.storage else {
            panic!("expected mock storage");
        };
        let sigs = mock.get_release_signatures(77).await.unwrap();
        assert_eq!(sigs.len(), 1, "owned claim persists exactly one signature");
        assert_eq!(sigs[0].0, Signature::default().to_string());
        assert_eq!(
            state.in_flight.len(),
            1,
            "owned claim broadcasts and stashes"
        );
        let after = mock.pending_transactions.lock().unwrap()[0].updated_at;
        assert_ne!(after, t_lock, "a successful claim bumps updated_at");
        assert_eq!(
            state.in_flight.entries.lock().unwrap()[0]
                .ctx
                .deposit_claim_lease,
            Some(after),
            "the in-flight context carries the next ownership lease"
        );
    }

    // ── spawn_fire_and_store: cap enforcement ─────────────────────────

    /// When the semaphore is exhausted (all MAX_IN_FLIGHT slots occupied),
    /// `spawn_fire_and_store` must return `false` without spawning any task
    /// or emitting any storage update. DB status stays unchanged so the
    /// fetcher can re-emit the transaction on the next poll cycle.
    #[tokio::test]
    async fn spawn_fire_and_store_cap_exhausted_returns_false() {
        let state = make_sender_state();

        // Hold all permits — simulates MAX_IN_FLIGHT tasks in-flight.
        let _permits: Vec<_> = (0..MAX_IN_FLIGHT)
            .map(|_| state.semaphore.clone().try_acquire_owned().unwrap())
            .collect();
        assert_eq!(state.semaphore.available_permits(), 0);

        let (storage_tx, mut storage_rx) = mpsc::channel(10);
        let ctx = TransactionContext {
            transaction_id: Some(9999),
            withdrawal_nonce: None,
            trace_id: None,
            deposit_claim_lease: None,
        };

        let result = spawn_fire_and_store(
            &state,
            dummy_instruction(),
            None,
            ctx,
            RetryPolicy::None,
            ExtraErrorCheckPolicy::None,
            storage_tx,
            SendDurability::Terminal,
        );

        assert!(!result, "must return false when at capacity");
        // Yield to ensure any erroneously spawned tasks have time to run.
        tokio::task::yield_now().await;
        assert!(storage_rx.try_recv().is_err(), "no storage update expected");
        // Queue stays empty — no entry pushed.
        assert!(state.in_flight.is_empty());
    }

    /// When capacity is available, `spawn_fire_and_store` must return `true` and
    /// the permit must be consumed immediately (before the RPC call completes),
    /// so back-pressure is applied as soon as the task starts, not after it finishes.
    #[tokio::test]
    async fn spawn_fire_and_store_available_capacity_returns_true_and_consumes_permit() {
        let state = make_sender_state();
        assert_eq!(state.semaphore.available_permits(), MAX_IN_FLIGHT);

        let (storage_tx, _storage_rx) = mpsc::channel(10);

        let result = spawn_fire_and_store(
            &state,
            dummy_instruction(),
            None,
            TransactionContext {
                transaction_id: Some(1),
                withdrawal_nonce: None,
                trace_id: None,
                deposit_claim_lease: None,
            },
            RetryPolicy::None,
            ExtraErrorCheckPolicy::None,
            storage_tx,
            SendDurability::Terminal,
        );

        assert!(result, "must return true when capacity is available");
        // Permit must be consumed before spawn returns — regardless of whether
        // the RPC call has completed yet.
        assert_eq!(
            state.semaphore.available_permits(),
            MAX_IN_FLIGHT - 1,
            "one permit must be held by the spawned task"
        );
    }

    // ── run_poll_task: cancellation ───────────────────────────────────

    /// Cancelling while the task is blocked waiting for entries (idle queue) must
    /// cause it to exit cleanly without hanging.
    #[tokio::test]
    async fn run_poll_task_cancels_while_waiting_for_entries() {
        let in_flight = InFlightQueue::new();
        let (result_tx, _result_rx) = mpsc::channel(8);
        let (storage_tx, _storage_rx) = mpsc::channel(8);
        let rpc = Arc::new(RpcClientWithRetry::with_retry_config(
            "http://localhost:8899".to_string(),
            RetryConfig::default(),
            CommitmentConfig::confirmed(),
        ));
        let token = tokio_util::sync::CancellationToken::new();

        let handle = tokio::spawn(run_poll_task(
            in_flight.clone(),
            result_tx,
            rpc,
            storage_tx,
            ProgramType::Escrow,
            50,
            token.clone(),
        ));

        // Cancel immediately — task is blocked on notified(), must wake and exit.
        token.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("task must exit within 2s after cancellation")
            .expect("task must not panic");
    }

    /// Cancelling while the task is sleeping between notify and drain must cause
    /// it to exit without processing any entries.
    #[tokio::test]
    async fn run_poll_task_cancels_during_poll_interval_sleep() {
        let in_flight = InFlightQueue::new();
        let (result_tx, _result_rx) = mpsc::channel(8);
        let (storage_tx, _storage_rx) = mpsc::channel(8);
        let rpc = Arc::new(RpcClientWithRetry::with_retry_config(
            "http://localhost:8899".to_string(),
            RetryConfig::default(),
            CommitmentConfig::confirmed(),
        ));
        let token = tokio_util::sync::CancellationToken::new();

        let handle = tokio::spawn(run_poll_task(
            in_flight.clone(),
            result_tx,
            rpc,
            storage_tx,
            ProgramType::Escrow,
            60_000, // very long interval — task will be sleeping here when we cancel
            token.clone(),
        ));

        // Push an entry to unblock the first select (notified), then cancel
        // while the task is in the poll_interval sleep.
        in_flight.push(make_in_flight_tx(Signature::new_unique(), 1));
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        token.cancel();

        tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("task must exit within 2s after cancellation")
            .expect("task must not panic");
    }

    /// When the result_tx receiver is dropped (sender loop gone), the task must
    /// detect the closed channel and exit cleanly rather than looping forever.
    #[tokio::test]
    async fn run_poll_task_exits_when_result_channel_closed() {
        let mut server = mockito::Server::new_async().await;

        // Return a confirmed-with-error status so a NeedsRouting result is produced,
        // which forces a send on result_tx (the closed channel) → task must exit.
        let sig = Signature::new_unique();
        let _m = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "method": "getSignatureStatuses"
            })))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "context": {"slot": 5},
                        "value": [{
                            "slot": 5,
                            "confirmations": null,
                            "confirmationStatus": "finalized",
                            "err": {"InstructionError": [0, "GenericError"]},
                            "status": {"Err": {"InstructionError": [0, "GenericError"]}}
                        }]
                    }
                })
                .to_string(),
            )
            .expect_at_least(1)
            .create();

        let in_flight = InFlightQueue::new();
        // Drop result_rx immediately to close the channel from the receiver side.
        let (result_tx, result_rx) = mpsc::channel::<Vec<PollTaskResult>>(8);
        drop(result_rx);
        let (storage_tx, _storage_rx) = mpsc::channel(8);
        let rpc = Arc::new(RpcClientWithRetry::with_retry_config(
            server.url(),
            RetryConfig {
                max_attempts: 1,
                base_delay: std::time::Duration::from_millis(1),
                max_delay: std::time::Duration::from_millis(1),
            },
            CommitmentConfig::confirmed(),
        ));
        let token = tokio_util::sync::CancellationToken::new();

        in_flight.push(make_in_flight_tx(sig, 42));

        let handle = tokio::spawn(run_poll_task(
            in_flight.clone(),
            result_tx,
            rpc,
            storage_tx,
            ProgramType::Escrow,
            1, // minimal sleep
            token.clone(),
        ));

        tokio::time::timeout(std::time::Duration::from_secs(3), handle)
            .await
            .expect("task must exit within 3s when result channel is closed")
            .expect("task must not panic");
    }

    // ── ambiguous-nonce gate ─────────────────────────────────────────

    fn ambiguous_pending_remint(nonce: u64) -> PendingRemint {
        PendingRemint {
            ctx: TransactionContext {
                transaction_id: Some(1),
                withdrawal_nonce: Some(nonce),
                trace_id: Some("t".to_string()),
                deposit_claim_lease: None,
            },
            remint_info: WithdrawalRemintInfo {
                transaction_id: 1,
                source_event_id: crate::operator::instruction_util::SourceEventId::new(
                    "remint-sig-1",
                    0,
                    None,
                ),
                trace_id: "t".to_string(),
                mint: Pubkey::new_unique(),
                user: Pubkey::new_unique(),
                user_ata: Pubkey::new_unique(),
                token_program: spl_token::id(),
                amount: 1000,
            },
            signatures: vec![],
            original_error: "x".to_string(),
            deadline: Utc::now(),
            finality_check_attempts: 0,
        }
    }

    /// The gate must block only withdrawals in the same tree as the unresolved
    /// nonce. Test config MAX_TREE_LEAVES = 8, so nonce 2 is tree 0.
    #[test]
    fn ambiguous_nonce_gate_is_tree_scoped() {
        let mut state = make_sender_state();
        state.pending_remints.push(ambiguous_pending_remint(2));

        assert!(state.has_unresolved_ambiguous_nonce(0), "same tree blocks");
        assert!(
            !state.has_unresolved_ambiguous_nonce(1),
            "other tree does not"
        );

        state.pending_remints.clear();
        assert!(
            !state.has_unresolved_ambiguous_nonce(0),
            "no ambiguous nonce, gate clear"
        );
    }

    /// A withdrawal blocked by the gate must be parked, not built or sent, and
    /// must leave the DB row untouched (no status update).
    #[tokio::test]
    async fn blocked_withdrawal_is_parked_not_sent() {
        let mut state = make_sender_state();
        let (storage_tx, mut storage_rx) = mpsc::channel(10);

        // Nonce 2 (tree 0) is unresolved, blocking tree 0.
        state.pending_remints.push(ambiguous_pending_remint(2));

        // Incoming withdrawal nonce 3 is in the same tree. The gate parks before
        // touching the builder, so an empty builder is fine. It carries remint_info
        // that must survive the park unchanged (the drain has no other source for it).
        let remint_info = make_remint_info(99);
        let tx_builder = TransactionBuilder::ReleaseFunds(Box::new(ReleaseFundsBuilderWithNonce {
            builder: ReleaseFundsBuilder::new(),
            nonce: 3,
            transaction_id: 99,
            trace_id: "trace-99".to_string(),
            remint_info: Some(remint_info.clone()),
        }));
        handle_transaction_submission(&mut state, tx_builder, &storage_tx).await;

        assert_eq!(state.ambiguous_retry_queue.len(), 1);
        assert_eq!(state.ambiguous_retry_queue[0].nonce, 3);
        assert_eq!(
            state.ambiguous_retry_queue[0].remint_info.as_ref(),
            Some(&remint_info),
            "remint_info must travel with the parked withdrawal unchanged"
        );
        assert!(
            storage_rx.try_recv().is_err(),
            "parked withdrawal must not emit a status update"
        );
    }
}
