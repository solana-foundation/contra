//! End-to-end coverage for `handle_confirmation_result`
//! (`indexer/src/operator/sender/transaction.rs`) via the
//! `test_hooks::handle_confirmation_result` wrapper.
//!
//! The function is the central error router for confirmed-but-failed
//! transactions. Each scenario synthesises a
//! `Result<ConfirmationResult, TransactionError>` corresponding to one
//! match arm and verifies the arm fires by inspecting the
//! `TransactionStatusUpdate` the production helper emits — every fatal
//! arm passes a distinct `error_msg` string into
//! `handle_permanent_failure → send_fatal_error`, so the per-arm route
//! is identifiable from the status update alone.
//!
//! Out of scope (covered separately):
//!   - `Ok(Confirmed)` arm — already exercised by the
//!     `sender_poll_rpc_error` success scenario via `handle_success`.
//!   - `Ok(Failed(InvalidSmtProof))` arm — needs an SMT state fixture.
//!   - `Ok(MintNotInitialized)` JIT-init arm — covered by `jit_mint_helper`.
//!   - `Ok(Retry) + Idempotent` arm — recursive `send_and_confirm`,
//!     covered transitively by the next iteration's wire scripting.

#[path = "sender_fixtures.rs"]
mod sender_fixtures;

use {
    private_channel_escrow_program_client::errors::PrivateChannelEscrowProgramError,
    private_channel_indexer::{
        error::{ProgramError, TransactionError},
        operator::{
            sender::{test_hooks, types::TransactionStatusUpdate},
            utils::{
                instruction_util::{ExtraErrorCheckPolicy, RetryPolicy},
                transaction_util::ConfirmationResult,
            },
        },
        storage::TransactionStatus,
    },
    sender_fixtures::{
        blockhash_reply, build_default_sender_state, confirmed_status_reply, deposit_ctx,
        make_instruction, send_transaction_echo_reply,
    },
    solana_sdk::signature::Signature,
};

/// Drive the hook with a synthesised `result` and return the single
/// status update the production code emits.
async fn drive_and_recv(
    result: Result<ConfirmationResult, TransactionError>,
    retry_policy: RetryPolicy,
    txn_id: i64,
) -> TransactionStatusUpdate {
    let (mut state, mut storage_rx, storage_tx, mock) = build_default_sender_state().await;
    let ctx = deposit_ctx(txn_id);
    test_hooks::handle_confirmation_result(
        &mut state,
        result,
        Signature::new_unique(),
        None,
        &ctx,
        make_instruction(),
        retry_policy,
        &ExtraErrorCheckPolicy::None,
        &storage_tx,
    )
    .await;
    let update = storage_rx
        .recv()
        .await
        .expect("fatal arm must emit a status update");
    mock.shutdown().await;
    update
}

// ─────────────────────────────────────────────────────────────────────
// InvalidTransactionNonceForCurrentTreeIndex — fatal arm.
// ─────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn invalid_transaction_nonce_routes_to_fatal_arm() {
    let result = Ok(ConfirmationResult::Failed(Some(
        PrivateChannelEscrowProgramError::InvalidTransactionNonceForCurrentTreeIndex,
    )));
    let update = drive_and_recv(result, RetryPolicy::Idempotent, 401).await;
    assert_eq!(update.transaction_id, 401);
    assert_eq!(update.status, TransactionStatus::Failed);
    assert_eq!(
        update.error_message.as_deref(),
        Some("Invalid nonce for tree index"),
        "the InvalidTransactionNonce arm must pass its specific error message; got {:?}",
        update.error_message
    );
}

// ─────────────────────────────────────────────────────────────────────
// Failed(Some(other)) — generic catch-all arm.
// ─────────────────────────────────────────────────────────────────────
//
// Any program error not specifically routed (InvalidSmtProof,
// InvalidTransactionNonce, MintNotInitialized) falls into the generic
// `Failed(program_error)` catch-all and is debug-formatted into the
// error message.
#[tokio::test]
async fn unmapped_program_error_routes_to_generic_failed_arm() {
    let result = Ok(ConfirmationResult::Failed(Some(
        PrivateChannelEscrowProgramError::InvalidMint,
    )));
    let update = drive_and_recv(result, RetryPolicy::Idempotent, 402).await;
    assert_eq!(update.status, TransactionStatus::Failed);
    let msg = update.error_message.unwrap_or_default();
    assert!(
        msg.contains("InvalidMint"),
        "generic Failed arm must debug-format the program error; got {msg:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────
// MintNotInitialized + no cached builder — fatal arm.
// ─────────────────────────────────────────────────────────────────────
//
// `state.mint_builders` is empty for the supplied txn_id, so the helper
// cannot attempt JIT initialization. The arm emits the
// "Unexpected mint error" error message before falling through to
// `send_fatal_error`.
#[tokio::test]
async fn mint_not_initialized_without_builder_routes_to_fatal_arm() {
    let result = Ok(ConfirmationResult::MintNotInitialized);
    let update = drive_and_recv(result, RetryPolicy::None, 403).await;
    assert_eq!(update.status, TransactionStatus::Failed);
    assert_eq!(
        update.error_message.as_deref(),
        Some("Unexpected mint error"),
        "MintNotInitialized without a cached builder must emit the 'Unexpected mint error' label"
    );
}

// ─────────────────────────────────────────────────────────────────────
// Retry + RetryPolicy::None — fatal arm.
// ─────────────────────────────────────────────────────────────────────
//
// Confirmation timeout on a non-idempotent operation: production cannot
// safely retry (status unknown), so the arm routes straight to
// `handle_permanent_failure` with a distinct
// "unsafe to retry" suffix.
#[tokio::test]
async fn retry_under_none_policy_routes_to_fatal_arm() {
    let result = Ok(ConfirmationResult::Retry);
    let update = drive_and_recv(result, RetryPolicy::None, 404).await;
    assert_eq!(update.status, TransactionStatus::Failed);
    let msg = update.error_message.unwrap_or_default();
    assert!(
        msg.contains("unsafe to retry"),
        "Retry+None must surface the 'unsafe to retry' label; got {msg:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────
// Err(TransactionError) — fatal arm.
// ─────────────────────────────────────────────────────────────────────
//
// A failure inside the polling layer (e.g. SMT-state unavailable
// surfacing as a ProgramError) routes through the catch-all `Err(e)`
// arm. The `error_msg` is the Display of the underlying error.
#[tokio::test]
async fn err_result_routes_to_confirmation_error_arm() {
    let result = Err(TransactionError::Program(ProgramError::SmtNotInitialized));
    let update = drive_and_recv(result, RetryPolicy::Idempotent, 405).await;
    assert_eq!(update.status, TransactionStatus::Failed);
    let msg = update.error_message.unwrap_or_default();
    // The underlying ProgramError::SmtNotInitialized has Display
    // "SMT not initialized"; whatever the exact wording, it must
    // not be empty and must not match any of the other arms' labels.
    assert!(
        !msg.is_empty(),
        "confirmation_error arm must surface the underlying error string"
    );
    assert!(
        !msg.contains("Invalid nonce")
            && !msg.contains("Unexpected mint")
            && !msg.contains("unsafe to retry"),
        "confirmation_error arm must not collide with another arm's error label; got {msg:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────
// Retry + RetryPolicy::Idempotent — recursive send_and_confirm succeeds.
// ─────────────────────────────────────────────────────────────────────
//
// `RetryPolicy::Idempotent` makes the Retry arm safe to re-send (the
// nonce protects against duplicates). `handle_confirmation_result`
// calls `send_and_confirm` recursively; with the wire scripts in
// place, the recursive call confirms successfully and emits a
// `Completed` status update — NOT a Failed one. This pins the
// "retry succeeded" branch as observably distinct from every fatal
// arm above.
#[tokio::test]
async fn retry_under_idempotent_policy_recursively_resends_and_completes() {
    let (mut state, mut storage_rx, storage_tx, mock) = build_default_sender_state().await;

    // Scripts for the recursive send_and_confirm call.
    mock.enqueue("getLatestBlockhash", blockhash_reply());
    mock.enqueue("sendTransaction", send_transaction_echo_reply());
    mock.enqueue("getSignatureStatuses", confirmed_status_reply());

    let ctx = deposit_ctx(406);
    test_hooks::handle_confirmation_result(
        &mut state,
        Ok(ConfirmationResult::Retry),
        Signature::new_unique(),
        None,
        &ctx,
        // Recursive send_and_confirm consumes its own instruction; the
        // call shape is otherwise identical to a fresh attempt.
        make_instruction(),
        RetryPolicy::Idempotent,
        &ExtraErrorCheckPolicy::None,
        &storage_tx,
    )
    .await;

    let update = storage_rx
        .recv()
        .await
        .expect("Idempotent retry must drive the recursive send to a Completed status");
    assert_eq!(update.transaction_id, 406);
    assert_eq!(
        update.status,
        TransactionStatus::Completed,
        "Idempotent retry must succeed via recursive send_and_confirm; got {:?}",
        update.status
    );
    // Recursive call hit the wire exactly once.
    assert_eq!(mock.call_count("sendTransaction"), 1);
    assert_eq!(mock.call_count("getSignatureStatuses"), 1);
    mock.shutdown().await;
}
