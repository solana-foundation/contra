//! End-to-end coverage for the deferred-remint flow
//! (`indexer/src/operator/sender/remint.rs`) via the
//! `test_hooks::{process_pending_remints, execute_deferred_remint}`
//! wrappers.
//!
//! The flow transitions a withdrawal row through Pending → PendingRemint
//! → either Completed (the withdrawal actually finalized after we
//! deferred), FailedReminted (the remint succeeded), or ManualReview
//! (both the withdrawal failed AND the remint failed). The four
//! scenarios pin the four observable terminal arms:
//!
//!   (a) **Idempotency short-circuit** — `attempt_remint`'s opening
//!       `find_existing_mint_signature_with_memo` lookup returns
//!       `Some(prior_signature)`, so the helper reports success without
//!       sending a duplicate remint. Drives the
//!       `execute_deferred_remint` happy-path-via-idempotency arm and
//!       emits `FailedReminted` carrying the prior sig.
//!
//!   (b) **Withdrawal actually finalized** — finality check returns a
//!       finalized success for one of the stashed signatures, so
//!       `process_pending_remints` skips the remint and emits
//!       `Completed` with the finalized sig as the
//!       counterpart_signature.
//!
//!   (c) **Finality-check RPC error, attempts < MAX** — entry is
//!       re-queued with a fresh deadline and `finality_check_attempts`
//!       incremented; no status update.
//!
//!   (d) **Finality-check RPC error, attempts == MAX-1** — next failure
//!       trips the cap and emits `ManualReview` with a "finality check
//!       failed" error message.

#[path = "sender_fixtures.rs"]
mod sender_fixtures;

use {
    contra_indexer::{
        config::ProgramType,
        operator::{
            sender::{
                test_hooks,
                types::{PendingRemint, SenderState, TransactionStatusUpdate},
            },
            utils::instruction_util::{remint_idempotency_memo, WithdrawalRemintInfo},
            SignerUtil,
        },
        storage::{common::storage::mock::MockStorage, Storage, TransactionStatus},
    },
    sender_fixtures::{
        blockhash_reply, confirmed_status_reply, ensure_admin_signer_env, make_config,
        make_remint_info, send_transaction_echo_reply, withdrawal_ctx,
    },
    serde_json::json,
    solana_keychain::SolanaSigner,
    solana_sdk::{commitment_config::CommitmentLevel, signature::Signature},
    std::{str::FromStr, sync::Arc},
    test_utils::mock_rpc::{MockRpcServer, Reply},
    tokio::sync::mpsc,
};

async fn build_state(
    rpc_url: String,
) -> (
    SenderState,
    mpsc::Receiver<TransactionStatusUpdate>,
    mpsc::Sender<TransactionStatusUpdate>,
) {
    ensure_admin_signer_env();
    let storage = Arc::new(Storage::Mock(MockStorage::new()));
    let state = test_hooks::new_sender_state(
        &make_config(rpc_url, ProgramType::Withdraw),
        CommitmentLevel::Confirmed,
        None,
        storage,
        1,
        // Tight confirmation poll interval — the remint flow's
        // `check_transaction_status` consumes this for any send path.
        1,
        None,
    )
    .expect("SenderState construction must succeed under Mock storage");
    let (storage_tx, storage_rx) = mpsc::channel(8);
    (state, storage_rx, storage_tx)
}

fn make_pending_remint(
    transaction_id: i64,
    nonce: u64,
    signatures: Vec<Signature>,
    finality_check_attempts: u32,
    info: WithdrawalRemintInfo,
) -> PendingRemint {
    PendingRemint {
        ctx: withdrawal_ctx(transaction_id, nonce),
        remint_info: info,
        signatures,
        original_error: "release_funds failed".to_string(),
        // Past deadline so `process_pending_remints` treats the entry
        // as matured and processes it on the first tick.
        deadline: chrono::Utc::now() - chrono::Duration::seconds(1),
        finality_check_attempts,
    }
}

// ─────────────────────────────────────────────────────────────────────
// (a) Idempotency short-circuit — attempt_remint finds prior confirmed remint.
// ─────────────────────────────────────────────────────────────────────
//
// Drives `execute_deferred_remint` directly. The first call inside
// `attempt_remint` is `find_existing_mint_signature_with_memo`, which
// scripts (`getSignaturesForAddress` + `getTransaction`) to return a
// prior confirmed remint carrying the matching memo. The helper
// short-circuits before sending a new transaction and routes the
// `Ok(prior_sig)` arm to the `FailedReminted` status emission.
#[tokio::test]
async fn execute_deferred_remint_short_circuits_on_prior_confirmed_remint() {
    let mock = MockRpcServer::start().await;
    let (state, mut storage_rx, storage_tx) = build_state(mock.url()).await;

    let txn_id: i64 = 7_777;
    let info = make_remint_info(txn_id);
    let memo = remint_idempotency_memo(txn_id);

    let prior_remint_sig = Signature::from_str(
        "4BxWw1FjwQCHXWkrK4ZehPWauFTPhBafSr9m8Cuht73LG73nUs3wfuJ6gigkhNppP4pYogP5pQDENbE5nQx1Qp4B",
    )
    .unwrap();

    // Phase 1 of attempt_remint: getSignaturesForAddress on the recipient ATA.
    mock.enqueue(
        "getSignaturesForAddress",
        Reply::result(json!([
            {
                "signature": prior_remint_sig.to_string(),
                "slot": 100u64,
                "err": null,
                "memo": format!("[5] {}", memo),
                "blockTime": 1_700_000_000i64,
                "confirmationStatus": "finalized",
            }
        ])),
    );

    // Phase 2 of attempt_remint: getTransaction on the matching sig
    // returns a parsed payload whose `mintTo` info matches the remint
    // builder exactly, so the idempotency short-circuit fires.
    let memo_program_id = "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr";
    let admin = SignerUtil::admin_signer().pubkey();
    mock.enqueue(
        "getTransaction",
        Reply::result(json!({
            "slot": 100,
            "blockTime": 1_700_000_000i64,
            "meta": {
                "err": null,
                "status": { "Ok": null },
                "fee": 5000u64,
                "innerInstructions": [],
                "preBalances": [1_000_000u64],
                "postBalances": [999_995u64],
                "logMessages": [],
                "preTokenBalances": [],
                "postTokenBalances": [],
                "rewards": [],
                "computeUnitsConsumed": 0u64,
            },
            "transaction": {
                "signatures": [prior_remint_sig.to_string()],
                "message": {
                    "accountKeys": [
                        { "pubkey": admin.to_string(),               "signer": true,  "writable": true,  "source": "transaction" },
                        { "pubkey": info.user_ata.to_string(),       "signer": false, "writable": true,  "source": "transaction" },
                        { "pubkey": info.mint.to_string(),           "signer": false, "writable": true,  "source": "transaction" },
                        { "pubkey": info.token_program.to_string(),  "signer": false, "writable": false, "source": "transaction" },
                        { "pubkey": memo_program_id,                 "signer": false, "writable": false, "source": "transaction" },
                    ],
                    "recentBlockhash": "GHtXQBsoZHjzkAm2Sdm6FTyFHBCqBnLanJJhZFCFJXoe",
                    "instructions": [
                        { "program": "spl-memo", "programId": memo_program_id, "parsed": memo },
                        {
                            "program": "spl-token",
                            "programId": info.token_program.to_string(),
                            "parsed": {
                                "type": "mintTo",
                                "info": {
                                    "mint": info.mint.to_string(),
                                    "account": info.user_ata.to_string(),
                                    "mintAuthority": admin.to_string(),
                                    "amount": info.amount.to_string(),
                                },
                            },
                        },
                    ],
                },
            },
        })),
    );

    let entry = make_pending_remint(txn_id, 7, vec![Signature::new_unique()], 0, info);
    test_hooks::execute_deferred_remint(&state, &entry, &storage_tx).await;

    let update = storage_rx
        .recv()
        .await
        .expect("idempotency short-circuit must emit a FailedReminted update");
    assert_eq!(update.transaction_id, txn_id);
    assert_eq!(update.status, TransactionStatus::FailedReminted);
    assert_eq!(
        update.remint_signature.as_deref(),
        Some(prior_remint_sig.to_string().as_str()),
        "remint_signature must echo the prior confirmed remint"
    );
    assert!(
        update.remint_attempted,
        "FailedReminted must mark remint_attempted=true"
    );
    // Critically: no `sendTransaction` call. The whole point of the
    // idempotency check is to avoid duplicate on-chain submissions.
    assert_eq!(
        mock.call_count("sendTransaction"),
        0,
        "idempotency match must skip the wire send entirely"
    );
    mock.shutdown().await;
}

// ─────────────────────────────────────────────────────────────────────
// (b) Withdrawal actually finalized — process_pending_remints emits Completed.
// ─────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn process_pending_remints_skips_remint_when_withdrawal_finalized() {
    let mock = MockRpcServer::start().await;
    let (mut state, mut storage_rx, storage_tx) = build_state(mock.url()).await;

    let withdrawal_sig = Signature::new_unique();
    state.pending_remints.push(make_pending_remint(
        91,
        3,
        vec![withdrawal_sig],
        0,
        make_remint_info(91),
    ));

    mock.enqueue(
        "getSignatureStatuses",
        Reply::result(json!({
            "context": { "slot": 200 },
            "value": [{
                "slot": 100,
                "confirmations": null,
                "err": null,
                "status": { "Ok": null },
                "confirmationStatus": "finalized"
            }]
        })),
    );

    test_hooks::process_pending_remints(&mut state, &storage_tx).await;

    let update = storage_rx
        .recv()
        .await
        .expect("finalized-withdrawal arm must emit a Completed update");
    assert_eq!(update.transaction_id, 91);
    assert_eq!(update.status, TransactionStatus::Completed);
    assert_eq!(
        update.counterpart_signature.as_deref(),
        Some(withdrawal_sig.to_string().as_str())
    );
    assert!(
        state.pending_remints.is_empty(),
        "entry must be consumed once Completed is emitted"
    );
    mock.shutdown().await;
}

// ─────────────────────────────────────────────────────────────────────
// (c) Finality-check RPC error, attempts < MAX → re-queue.
// ─────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn process_pending_remints_requeues_on_finality_check_rpc_error() {
    let mock = MockRpcServer::start().await;
    let (mut state, mut storage_rx, storage_tx) = build_state(mock.url()).await;

    state.pending_remints.push(make_pending_remint(
        92,
        4,
        vec![Signature::new_unique()],
        0,
        make_remint_info(92),
    ));

    // The RpcClientWithRetry default retry loop is wired on top of the
    // raw call. Five errors guarantee the wrapper exhausts and surfaces
    // the error to `process_pending_remints` regardless of retry budget.
    mock.enqueue_sequence(
        "getSignatureStatuses",
        vec![
            Reply::error(-32000, "rpc dead 1"),
            Reply::error(-32000, "rpc dead 2"),
            Reply::error(-32000, "rpc dead 3"),
            Reply::error(-32000, "rpc dead 4"),
            Reply::error(-32000, "rpc dead 5"),
        ],
    );

    test_hooks::process_pending_remints(&mut state, &storage_tx).await;

    assert!(
        storage_rx.try_recv().is_err(),
        "no status update on the re-queue branch"
    );
    assert_eq!(
        state.pending_remints.len(),
        1,
        "entry must be re-queued, not consumed"
    );
    assert_eq!(
        state.pending_remints[0].finality_check_attempts, 1,
        "finality_check_attempts must increment by 1 per failed cycle"
    );
    mock.shutdown().await;
}

// ─────────────────────────────────────────────────────────────────────
// (d) Finality-check RPC error at MAX-1 attempts → ManualReview.
// ─────────────────────────────────────────────────────────────────────
//
// MAX_FINALITY_CHECK_ATTEMPTS is 3. An entry already at attempts=2
// (one less than MAX) hits the cap on the next failure and routes to
// ManualReview rather than re-queueing.
#[tokio::test]
async fn process_pending_remints_routes_to_manual_review_at_max_attempts() {
    let mock = MockRpcServer::start().await;
    let (mut state, mut storage_rx, storage_tx) = build_state(mock.url()).await;

    state.pending_remints.push(make_pending_remint(
        93,
        5,
        vec![Signature::new_unique()],
        2, // MAX_FINALITY_CHECK_ATTEMPTS - 1
        make_remint_info(93),
    ));

    mock.enqueue_sequence(
        "getSignatureStatuses",
        vec![
            Reply::error(-32000, "rpc dead 1"),
            Reply::error(-32000, "rpc dead 2"),
            Reply::error(-32000, "rpc dead 3"),
            Reply::error(-32000, "rpc dead 4"),
            Reply::error(-32000, "rpc dead 5"),
        ],
    );

    test_hooks::process_pending_remints(&mut state, &storage_tx).await;

    let update = storage_rx
        .recv()
        .await
        .expect("max-attempts arm must emit a ManualReview update");
    assert_eq!(update.transaction_id, 93);
    assert_eq!(update.status, TransactionStatus::ManualReview);
    let msg = update.error_message.unwrap_or_default();
    assert!(
        msg.contains("finality check failed"),
        "ManualReview at MAX must surface the 'finality check failed' label; got {msg:?}"
    );
    assert!(
        msg.contains("release_funds failed"),
        "ManualReview must preserve the original withdrawal error; got {msg:?}"
    );
    assert!(
        state.pending_remints.is_empty(),
        "entry must NOT be re-queued past the cap"
    );
    mock.shutdown().await;
}

// ─────────────────────────────────────────────────────────────────────
// (e) attempt_remint send + confirm path — emits FailedReminted with new sig.
// ─────────────────────────────────────────────────────────────────────
//
// Idempotency lookup returns empty, so `attempt_remint` proceeds to
// build instructions, sign-and-send, and check confirmation. With all
// three RPC calls scripted to succeed, the helper returns
// `Ok(new_sig)` and `execute_deferred_remint` emits FailedReminted
// carrying the freshly-minted remint signature (NOT a prior idempotent
// match).
#[tokio::test]
async fn execute_deferred_remint_emits_failed_reminted_after_successful_send() {
    let mock = MockRpcServer::start().await;
    let (state, mut storage_rx, storage_tx) = build_state(mock.url()).await;

    let txn_id: i64 = 7_001;
    let info = make_remint_info(txn_id);

    // Idempotency lookup: no prior remint.
    mock.enqueue("getSignaturesForAddress", Reply::result(json!([])));
    // Send + confirm: full happy path.
    mock.enqueue("getLatestBlockhash", blockhash_reply());
    mock.enqueue("sendTransaction", send_transaction_echo_reply());
    mock.enqueue("getSignatureStatuses", confirmed_status_reply());

    let entry = make_pending_remint(txn_id, 31, vec![Signature::new_unique()], 0, info);
    test_hooks::execute_deferred_remint(&state, &entry, &storage_tx).await;

    let update = storage_rx
        .recv()
        .await
        .expect("successful remint must emit a FailedReminted update");
    assert_eq!(update.transaction_id, txn_id);
    assert_eq!(update.status, TransactionStatus::FailedReminted);
    assert!(
        update.remint_signature.is_some(),
        "FailedReminted must carry the new remint signature"
    );
    assert!(
        update.remint_attempted,
        "FailedReminted must mark remint_attempted=true"
    );
    // Critically: every wire step ran. No reply queued unconsumed.
    assert_eq!(mock.call_count("sendTransaction"), 1);
    assert_eq!(mock.call_count("getSignatureStatuses"), 1);
    mock.shutdown().await;
}

// ─────────────────────────────────────────────────────────────────────
// (f) attempt_remint send fails — ManualReview combined error.
// ─────────────────────────────────────────────────────────────────────
//
// Idempotency lookup returns empty; `sendTransaction` returns a
// permanent error. `attempt_remint` returns `Err`, and
// `execute_deferred_remint` takes the failure arm — emits
// ManualReview with the combined "<original_error> | remint failed:
// <send_error>" message.
#[tokio::test]
async fn execute_deferred_remint_emits_manual_review_when_send_fails() {
    let mock = MockRpcServer::start().await;
    let (state, mut storage_rx, storage_tx) = build_state(mock.url()).await;

    let txn_id: i64 = 7_002;
    let info = make_remint_info(txn_id);

    mock.enqueue("getSignaturesForAddress", Reply::result(json!([])));
    mock.enqueue("getLatestBlockhash", blockhash_reply());
    mock.enqueue("sendTransaction", Reply::error(-32601, "method not found"));

    let entry = make_pending_remint(txn_id, 32, vec![Signature::new_unique()], 0, info);
    test_hooks::execute_deferred_remint(&state, &entry, &storage_tx).await;

    let update = storage_rx
        .recv()
        .await
        .expect("send-failure arm must emit a ManualReview update");
    assert_eq!(update.transaction_id, txn_id);
    assert_eq!(update.status, TransactionStatus::ManualReview);
    let msg = update.error_message.unwrap_or_default();
    assert!(
        msg.contains("remint failed"),
        "ManualReview message must surface the 'remint failed' label; got {msg:?}"
    );
    assert!(
        msg.contains("release_funds failed"),
        "ManualReview message must preserve the original withdrawal error; got {msg:?}"
    );
    assert!(
        update.remint_attempted,
        "send-failure arm must mark remint_attempted=true (we tried)"
    );
    assert!(
        update.remint_signature.is_none(),
        "no remint signature when the send itself failed"
    );
    mock.shutdown().await;
}
