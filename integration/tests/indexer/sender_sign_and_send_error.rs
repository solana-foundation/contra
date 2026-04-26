//! End-to-end coverage for the RPC-send-error arm of `send_and_confirm`
//! (`indexer/src/operator/sender/transaction.rs`) via the
//! `test_hooks::run_send_and_confirm` wrapper.
//!
//! Drives the production helper against a scripted `MockRpcServer`, so
//! both `send_and_confirm` and `handle_permanent_failure` (the
//! `Err`-arm fallthrough) execute in full. The two scenarios pin the two
//! observable shapes the production code treats as "permanent send
//! failure":
//!
//!   (a) `RetryPolicy::None` — the branch a Mint (Deposit) path uses —
//!       bubbles a transient -32000 RPC error straight to
//!       `handle_permanent_failure` after exactly one attempt.
//!
//!   (b) `RetryPolicy::Idempotent` — used by withdrawals — short-circuits
//!       the retry loop on a permanent -32601 error and routes to the
//!       same fatal arm after one attempt.
//!
//! Both paths emit a `TransactionStatus::Failed` update on `storage_tx`
//! (since these scenarios omit `withdrawal_nonce`, the remint-deferral
//! branch in `handle_permanent_failure` is not reached and the helper
//! falls through to `send_fatal_error`).

#[path = "sender_fixtures.rs"]
mod sender_fixtures;

use {
    contra_indexer::{
        operator::{
            sender::test_hooks,
            utils::instruction_util::{ExtraErrorCheckPolicy, RetryPolicy},
        },
        storage::TransactionStatus,
    },
    sender_fixtures::{
        blockhash_reply, build_default_sender_state, confirmed_status_reply, deposit_ctx,
        make_instruction, send_transaction_echo_reply,
    },
    test_utils::mock_rpc::Reply,
};

// ─────────────────────────────────────────────────────────────────────
// Happy path — full success branch through to Completed status.
// ─────────────────────────────────────────────────────────────────────
//
// Drives the `Ok(signature) → check_transaction_status → Confirmed →
// handle_success` arm of `send_and_confirm`. None of the failure-arm
// tests reach this branch (they all fail at `sendTransaction`), so
// without this scenario lines 302–347 of `transaction.rs` (the entire
// success branch) and the Mint path of `handle_success` stay at
// `DA:0`. Setup mirrors a Mint (deposit) flow: no `withdrawal_nonce`
// so `pending_signatures` isn't touched and `handle_success` takes
// the `Completed` direct-emit path.
#[tokio::test]
async fn happy_path_emits_completed_status() {
    let (mut state, mut storage_rx, storage_tx, mock) = build_default_sender_state().await;

    mock.enqueue("getLatestBlockhash", blockhash_reply());
    mock.enqueue("sendTransaction", send_transaction_echo_reply());
    mock.enqueue("getSignatureStatuses", confirmed_status_reply());

    let ctx = deposit_ctx(303);
    test_hooks::run_send_and_confirm(
        &mut state,
        make_instruction(),
        None,
        &ctx,
        RetryPolicy::None,
        &ExtraErrorCheckPolicy::None,
        &storage_tx,
    )
    .await;

    let update = storage_rx
        .recv()
        .await
        .expect("the success branch must emit a Completed status update");
    assert_eq!(update.transaction_id, 303);
    assert_eq!(update.status, TransactionStatus::Completed);
    // counterpart_signature must echo the tx's own signature.
    assert!(
        update.counterpart_signature.is_some(),
        "Completed update must carry the confirmed signature"
    );
    assert_eq!(mock.call_count("sendTransaction"), 1);
    assert_eq!(mock.call_count("getSignatureStatuses"), 1);
    mock.shutdown().await;
}

// ─────────────────────────────────────────────────────────────────────
// RetryPolicy::None — single-attempt send, transient error surfaces as Failed.
// ─────────────────────────────────────────────────────────────────────
//
// The Mint (Deposit) path uses `RetryPolicy::None`: one attempt, no
// retries even for transient codes. The error routes through
// `handle_permanent_failure` → `send_fatal_error` (no `withdrawal_nonce`)
// and emits a `TransactionStatus::Failed` update.
#[tokio::test]
async fn none_policy_routes_send_error_to_failed_status() {
    let (mut state, mut storage_rx, storage_tx, mock) = build_default_sender_state().await;

    mock.enqueue("getLatestBlockhash", blockhash_reply());
    mock.enqueue(
        "sendTransaction",
        Reply::error(-32000, "simulated server error"),
    );

    let ctx = deposit_ctx(101);
    test_hooks::run_send_and_confirm(
        &mut state,
        make_instruction(),
        None,
        &ctx,
        RetryPolicy::None,
        &ExtraErrorCheckPolicy::None,
        &storage_tx,
    )
    .await;

    let update = storage_rx
        .recv()
        .await
        .expect("permanent failure must emit a status update");
    assert_eq!(update.transaction_id, 101);
    assert_eq!(update.status, TransactionStatus::Failed);
    assert!(
        update
            .error_message
            .as_deref()
            .unwrap_or("")
            .to_lowercase()
            .contains("simulated"),
        "error_message must surface the underlying RPC error; got {:?}",
        update.error_message
    );
    assert_eq!(
        mock.call_count("sendTransaction"),
        1,
        "RetryPolicy::None must issue exactly one attempt"
    );
    mock.shutdown().await;
}

// ─────────────────────────────────────────────────────────────────────
// RetryPolicy::Idempotent — permanent error short-circuits the retry loop.
// ─────────────────────────────────────────────────────────────────────
//
// `is_permanent_rpc_error` classifies -32601 (method-not-found) as
// permanent, so the retry wrapper stops after the first attempt and the
// helper takes the same `handle_permanent_failure` path as the
// `RetryPolicy::None` test above. With no `withdrawal_nonce`, the
// remint-deferral branch is skipped and `send_fatal_error` emits a
// `Failed` status update.
#[tokio::test]
async fn idempotent_permanent_error_short_circuits_to_failed_status() {
    let (mut state, mut storage_rx, storage_tx, mock) = build_default_sender_state().await;

    mock.enqueue("getLatestBlockhash", blockhash_reply());
    mock.enqueue("sendTransaction", Reply::error(-32601, "method not found"));

    let ctx = deposit_ctx(202);
    test_hooks::run_send_and_confirm(
        &mut state,
        make_instruction(),
        None,
        &ctx,
        RetryPolicy::Idempotent,
        &ExtraErrorCheckPolicy::None,
        &storage_tx,
    )
    .await;

    let update = storage_rx
        .recv()
        .await
        .expect("permanent failure must emit a status update");
    assert_eq!(update.transaction_id, 202);
    assert_eq!(update.status, TransactionStatus::Failed);
    assert_eq!(
        mock.call_count("sendTransaction"),
        1,
        "permanent error classifier must short-circuit the retry loop on the first attempt"
    );
    mock.shutdown().await;
}
