//! End-to-end coverage for the sender-level retry counter inside
//! `send_and_confirm` (`indexer/src/operator/sender/transaction.rs`)
//! via the `test_hooks::run_send_and_confirm` wrapper.
//!
//! The retry counter only fires for the
//! `RetryPolicy::Idempotent` + `withdrawal_nonce` combination, the
//! withdrawal path. Each entry into `send_and_confirm` increments
//! `state.retry_counts[nonce]`; once that count reaches
//! `state.retry_max_attempts`, the next entry short-circuits, increments
//! the `max_retries_exceeded` metric, and routes to
//! `handle_permanent_failure` without attempting the wire send.
//!
//! The re-entry that makes the counter accumulate is the confirmation
//! retry: a broadcast the cluster accepts but never confirms polls to
//! exhaustion, `check_transaction_status` answers
//! `ConfirmationResult::Retry`, and the `Idempotent` arm of
//! `handle_confirmation_result` calls `send_and_confirm` again. Nothing
//! on that route is terminal, so nothing clears the counter between
//! resends. The cap tests below drive exactly that loop from a single
//! call and let the recursion supply the rest.
//!
//! Because the test omits the on-chain machinery a real withdrawal
//! requires (instance PDA, withdrawal bitmap, etc.), we construct the
//! `TransactionContext` and `InstructionWithSigners` directly and rely
//! on the same `MockRpcServer` plumbing the JIT and sign-and-send tests
//! use. With no `remint_cache` entry seeded in `state`, the
//! `handle_permanent_failure` arm falls through to `send_fatal_error`
//! and emits `TransactionStatus::Failed`.

#[path = "sender_fixtures.rs"]
mod sender_fixtures;

use {
    private_channel_indexer::{
        config::ProgramType,
        operator::{
            sender::{
                test_hooks,
                types::{PendingSig, SenderState, TransactionStatusUpdate},
            },
            utils::{
                instruction_util::{ExtraErrorCheckPolicy, RetryPolicy},
                transaction_util::MAX_POLL_ATTEMPTS_CONFIRMATION,
            },
        },
        storage::{common::storage::mock::MockStorage, Storage, TransactionStatus},
    },
    sender_fixtures::{
        blockhash_reply, ensure_admin_signer_env, make_config, make_instruction, make_remint_info,
        null_status_reply, send_transaction_echo_reply, withdrawal_ctx,
    },
    solana_sdk::{commitment_config::CommitmentLevel, signature::Signature},
    std::sync::Arc,
    test_utils::mock_rpc::{MockRpcServer, Reply},
    tokio::sync::mpsc,
};

/// Build a fresh withdrawal-side `SenderState` with the given
/// `retry_max_attempts`, plus the `(storage_tx, storage_rx)` pair the
/// helper writes status updates to. Also returns the `MockStorage`
/// handle for fault-injection scenarios — the inner `Arc<Mutex<...>>`
/// fields are shared, so `set_should_fail` calls on the returned
/// handle propagate to the storage that `SenderState` holds.
async fn build_fixture(
    retry_max_attempts: u32,
) -> (
    SenderState,
    mpsc::Receiver<TransactionStatusUpdate>,
    mpsc::Sender<TransactionStatusUpdate>,
    MockRpcServer,
    MockStorage,
) {
    ensure_admin_signer_env();
    let mock = MockRpcServer::start().await;
    let mock_storage = MockStorage::new();
    let storage = Arc::new(Storage::Mock(mock_storage.clone()));
    let state = test_hooks::new_sender_state(
        &make_config(mock.url(), ProgramType::Withdraw),
        CommitmentLevel::Confirmed,
        None,
        storage,
        retry_max_attempts,
        1,
        None,
    )
    .expect("SenderState construction must succeed under Mock storage");
    let (storage_tx, storage_rx) = mpsc::channel(16);
    (state, storage_rx, storage_tx, mock, mock_storage)
}

/// Seed `state.remint_cache[nonce]` so `handle_permanent_failure`
/// takes the deferred-remint branch instead of falling through to
/// `send_fatal_error`. The mint/user/ATA fields are not inspected by
/// the deferral path itself — they only matter once
/// `attempt_remint` actually runs (covered by `remint_flow.rs`).
fn seed_remint_cache(state: &mut SenderState, transaction_id: i64, nonce: u64) {
    state
        .remint_cache
        .insert(nonce, make_remint_info(transaction_id));
}

/// Helper: enqueue one (`getLatestBlockhash`, `sendTransaction`-error)
/// pair so a single `send_and_confirm` call exhausts the wire layer
/// quickly. -32601 is classified permanent → no inner retries → exactly
/// one `sendTransaction` per outer call.
fn enqueue_failing_send(mock: &MockRpcServer, label: &str) {
    mock.enqueue("getLatestBlockhash", blockhash_reply());
    mock.enqueue("sendTransaction", Reply::error(-32601, label.to_string()));
}

/// Number of `getSignatureStatuses` calls one unconfirmed send spends
/// before `check_transaction_status` gives up and answers `Retry`.
const POLLS_PER_SEND: usize = MAX_POLL_ATTEMPTS_CONFIRMATION as usize;

/// Enqueue one full "accepted but never confirms" cycle: the blockhash
/// fetch, a broadcast the mock accepts, and the null status replies that
/// drive `check_transaction_status` to `ConfirmationResult::Retry`. That
/// verdict is what re-enters `send_and_confirm` under
/// `RetryPolicy::Idempotent`, so one cycle equals one turn of the
/// production resend loop.
fn enqueue_unconfirmed_send(mock: &MockRpcServer) {
    mock.enqueue("getLatestBlockhash", blockhash_reply());
    mock.enqueue("sendTransaction", send_transaction_echo_reply());
    for _ in 0..POLLS_PER_SEND {
        mock.enqueue("getSignatureStatuses", null_status_reply());
    }
}

// ---------------------------------------------------------------------
// Sender retry counter: cap at `retry_max_attempts`, then short-circuit.
// ---------------------------------------------------------------------
//
// With `retry_max_attempts = 3` a single call is enough: every broadcast
// is accepted and never confirms, so each confirmation ends in
// `ConfirmationResult::Retry` and the `Idempotent` arm re-enters
// `send_and_confirm` without passing through any terminal cleanup. The
// first three entries each spend one wire send; the fourth observes
// `retry_counts[nonce] == 3 >= retry_max_attempts`, increments the
// `max_retries_exceeded` metric, and routes to
// `handle_permanent_failure` without touching the RPC. The fourth
// scripted cycle stays queued, which is what separates "the loop stopped"
// from "the loop ran out of script".
#[tokio::test]
async fn idempotent_send_loops_capped_by_retry_max_attempts() {
    let (mut state, mut storage_rx, storage_tx, mock, _mock_storage) = build_fixture(3).await;
    let ctx = withdrawal_ctx(404, 7);

    // Three cycles the resend loop may spend, plus one the cap must leave alone.
    for _ in 0..4 {
        enqueue_unconfirmed_send(&mock);
    }

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

    // Wire layer: exactly three `sendTransaction` calls, not four.
    assert_eq!(
        mock.call_count("sendTransaction"),
        3,
        "the fourth entry must short-circuit before issuing any wire send"
    );
    // Every send polled to exhaustion, so the loop turned on Retry and nothing else.
    assert_eq!(
        mock.call_count("getSignatureStatuses"),
        3 * POLLS_PER_SEND,
        "each of the three sends must poll to exhaustion before re-entering"
    );
    // The fourth scripted cycle must remain queued.
    assert_eq!(
        mock.remaining_scripted("sendTransaction"),
        1,
        "the fourth scripted reply must remain unconsumed"
    );

    // The cap is a terminal transition, so the nonce's caches are dropped
    // with it. A counter left behind would be spent budget charged to
    // whatever withdrawal reuses the nonce next, so its absence is part of
    // what the cap has to guarantee.
    assert!(
        !state.retry_counts.contains_key(&7),
        "the terminal transition must clear the nonce's retry counter"
    );

    // Only the short-circuit is terminal here: the three Retry verdicts
    // recursed instead of reporting, so exactly one update reaches
    // storage. With no `remint_cache` entry seeded,
    // `handle_permanent_failure` falls through to `send_fatal_error` and
    // that update carries the "Max retries exceeded" label.
    let mut updates = Vec::new();
    while let Ok(u) = storage_rx.try_recv() {
        updates.push(u);
    }
    assert_eq!(
        updates.len(),
        1,
        "only the capped entry is terminal, so it must emit the only status update"
    );
    assert_eq!(updates[0].status, TransactionStatus::Failed);
    assert!(
        updates[0]
            .error_message
            .as_deref()
            .map(|m| m.contains("Max retries"))
            .unwrap_or(false),
        "the update must surface the Max-retries-exceeded label; got {:?}",
        updates[0].error_message.as_deref()
    );

    mock.shutdown().await;
}

// ---------------------------------------------------------------------
// Higher-budget boundary: the counter still trips on the (n+1)th entry.
// ---------------------------------------------------------------------
//
// Same shape with `retry_max_attempts = 4`: four wire sends consumed by
// the resend loop, the fifth entry short-circuits. Pins the inclusive
// boundary of the retry-counter check (`attempts >= retry_max_attempts`)
// at a budget the first test cannot distinguish from a hard-coded three.
#[tokio::test]
async fn idempotent_send_loops_capped_at_higher_budget() {
    let (mut state, mut storage_rx, storage_tx, mock, _mock_storage) = build_fixture(4).await;
    let ctx = withdrawal_ctx(505, 11);

    for _ in 0..5 {
        enqueue_unconfirmed_send(&mock);
    }

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

    assert_eq!(
        mock.call_count("sendTransaction"),
        4,
        "the fifth entry must short-circuit before issuing any wire send"
    );
    assert_eq!(
        mock.call_count("getSignatureStatuses"),
        4 * POLLS_PER_SEND,
        "each of the four sends must poll to exhaustion before re-entering"
    );
    assert_eq!(
        mock.remaining_scripted("sendTransaction"),
        1,
        "the fifth scripted reply must remain unconsumed"
    );
    assert!(
        !state.retry_counts.contains_key(&11),
        "the terminal transition must clear the nonce's retry counter"
    );

    let mut updates = Vec::new();
    while let Ok(u) = storage_rx.try_recv() {
        updates.push(u);
    }
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].status, TransactionStatus::Failed);
    assert!(updates[0]
        .error_message
        .as_deref()
        .map(|m| m.contains("Max retries"))
        .unwrap_or(false));
    mock.shutdown().await;
}

// ─────────────────────────────────────────────────────────────────────
// Withdrawal deferral — zero stashed signatures → ManualReview.
// ─────────────────────────────────────────────────────────────────────
//
// `remint_cache[nonce]` is seeded so `handle_permanent_failure` enters
// the deferred-remint branch instead of falling through to
// `send_fatal_error`. `pending_signatures[nonce]` is left empty: the
// production code treats this as "the RPC may have broadcast the tx
// before erroring — blind remint is unsafe" and routes to
// `ManualReview` with the "no signatures to verify" label.
#[tokio::test]
async fn deferral_with_zero_stashed_signatures_routes_to_manual_review() {
    let (mut state, mut storage_rx, storage_tx, mock, _mock_storage) = build_fixture(3).await;
    let ctx = withdrawal_ctx(601, 21);
    seed_remint_cache(&mut state, 601, 21);
    // pending_signatures intentionally NOT seeded.

    enqueue_failing_send(&mock, "permanent send error");

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
        .expect("zero-sigs deferral arm must emit a ManualReview update");
    assert_eq!(update.transaction_id, 601);
    assert_eq!(update.status, TransactionStatus::ManualReview);
    let msg = update.error_message.unwrap_or_default();
    assert!(
        msg.contains("no signatures to verify"),
        "zero-sigs arm must surface the 'no signatures to verify' label; got {msg:?}"
    );
    assert!(
        !update.remint_attempted,
        "zero-sigs arm must NOT mark remint_attempted (no remint was scheduled)"
    );
    // Entry was NOT pushed to pending_remints — the unsafe-remint guard
    // returned ManualReview directly.
    assert!(state.pending_remints.is_empty());
    mock.shutdown().await;
}

// ─────────────────────────────────────────────────────────────────────
// Withdrawal deferral — set_pending_remint succeeds → push to queue.
// ─────────────────────────────────────────────────────────────────────
//
// Pre-seed `pending_signatures[nonce]` with one fake signature so the
// non-zero-sigs branch fires, calls `storage.set_pending_remint`
// (succeeds under default `MockStorage`), and pushes a `PendingRemint`
// entry into `state.pending_remints` for the deferred-finality-check
// loop to pick up later. No status update is emitted on this path —
// the row stays Processing until the remint resolves.
#[tokio::test]
async fn deferral_with_stashed_signatures_pushes_pending_remint() {
    let (mut state, mut storage_rx, storage_tx, mock, _mock_storage) = build_fixture(3).await;
    let ctx = withdrawal_ctx(602, 22);
    seed_remint_cache(&mut state, 602, 22);
    state.pending_signatures.insert(
        22,
        vec![PendingSig {
            signature: Signature::new_unique(),
            last_valid_block_height: 0,
        }],
    );

    enqueue_failing_send(&mock, "permanent send error");

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

    // No status update — the row is paused, not failed.
    assert!(
        storage_rx.try_recv().is_err(),
        "the push-to-pending_remints arm must NOT emit a status update; the row stays Processing until the deferred check runs"
    );
    assert_eq!(
        state.pending_remints.len(),
        1,
        "exactly one PendingRemint must be queued"
    );
    let entry = &state.pending_remints[0];
    assert_eq!(entry.ctx.transaction_id, Some(602));
    assert_eq!(entry.ctx.withdrawal_nonce, Some(22));
    assert_eq!(
        entry.signatures.len(),
        1,
        "the seeded stashed signature must be carried over to the PendingRemint entry"
    );
    mock.shutdown().await;
}

// ─────────────────────────────────────────────────────────────────────
// Withdrawal deferral — set_pending_remint fails → ManualReview.
// ─────────────────────────────────────────────────────────────────────
//
// Same setup as the success scenario above, but
// `MockStorage::set_should_fail("set_pending_remint", true)` makes the
// storage call fail. The arm catches the error and routes to
// `ManualReview` with a `"failed to persist pending remint"` label
// instead of leaving the row in a broken half-state.
#[tokio::test]
async fn deferral_set_pending_remint_storage_failure_routes_to_manual_review() {
    let (mut state, mut storage_rx, storage_tx, mock, mock_storage) = build_fixture(3).await;
    let ctx = withdrawal_ctx(603, 23);
    seed_remint_cache(&mut state, 603, 23);
    state.pending_signatures.insert(
        23,
        vec![PendingSig {
            signature: Signature::new_unique(),
            last_valid_block_height: 0,
        }],
    );
    mock_storage.set_should_fail("set_pending_remint", true);

    enqueue_failing_send(&mock, "permanent send error");

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
        .expect("storage-failure arm must emit a ManualReview update");
    assert_eq!(update.transaction_id, 603);
    assert_eq!(update.status, TransactionStatus::ManualReview);
    let msg = update.error_message.unwrap_or_default();
    assert!(
        msg.contains("failed to persist pending remint"),
        "storage-failure arm must surface the persistence-error label; got {msg:?}"
    );
    // The row must NOT have been pushed to pending_remints — the storage
    // write failed, so the in-memory queue cannot be allowed to drift
    // ahead of the durable state.
    assert!(state.pending_remints.is_empty());
    mock.shutdown().await;
}
