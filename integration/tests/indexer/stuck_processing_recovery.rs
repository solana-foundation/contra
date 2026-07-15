//! E2E tests for the stuck-`Processing` recovery worker.

#[path = "sender_fixtures.rs"]
mod sender_fixtures;

use {
    chrono::{Duration as ChronoDuration, Utc},
    private_channel_indexer::{
        config::ProgramType,
        metrics::{OPERATOR_STALE_PROCESSING_RECOVERED, OPERATOR_TRANSACTION_ERRORS},
        operator::{
            recovery::test_hooks,
            sender::{test_hooks as sender_hooks, types::SendDurability, types::SenderState},
            utils::instruction_util::{ExtraErrorCheckPolicy, MintToBuilder, RetryPolicy},
            utils::rpc_util::{RetryConfig, RpcClientWithRetry},
            utils::transaction_util::ConfirmationResult,
            SignerUtil, TransactionStatusUpdate,
        },
        storage::{common::models::DbTransactionBuilder, PostgresDb, Storage, TransactionType},
        PostgresConfig,
    },
    sender_fixtures::{
        account_info_reply_bytes, blockhash_reply, deposit_ctx, deposit_ctx_with_lease,
        make_config, make_instruction, pack_mint_with_authority, send_transaction_echo_reply,
    },
    serde_json::json,
    solana_keychain::SolanaSigner,
    solana_sdk::{
        commitment_config::{CommitmentConfig, CommitmentLevel},
        pubkey::Pubkey,
        signature::Signature,
    },
    std::{sync::Arc, time::Duration},
    test_utils::mock_rpc::{MockRpcServer, Reply},
    tokio::sync::mpsc,
};

/// Pre-test reading of a recovery-metric cell; assert `>snapshot` after.
fn snapshot_recovered(program: &str, outcome: &str, txn_type: &str) -> f64 {
    OPERATOR_STALE_PROCESSING_RECOVERED
        .with_label_values(&[program, outcome, txn_type])
        .get()
}

fn assert_recovered_increment(
    program: &str,
    outcome: &str,
    txn_type: &str,
    before: f64,
    label: &str,
) {
    let after = OPERATOR_STALE_PROCESSING_RECOVERED
        .with_label_values(&[program, outcome, txn_type])
        .get();
    assert!(
        after > before,
        "{label}: OPERATOR_STALE_PROCESSING_RECOVERED{{program={program},outcome={outcome},type={txn_type}}} \
         should have incremented (before={before}, after={after})"
    );
}

// ── fixture helpers ─────────────────────────────────────────────────────────

async fn start_pg(
    db_name: &str,
) -> (
    PostgresDb,
    String,
    testcontainers::ContainerAsync<testcontainers_modules::postgres::Postgres>,
) {
    use testcontainers::runners::AsyncRunner;
    use testcontainers_modules::postgres::Postgres;

    let container = Postgres::default()
        .with_db_name(db_name)
        .with_user("postgres")
        .with_password("password")
        .start()
        .await
        .expect("postgres container");
    let host = container.get_host().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:password@{}:{}/{}", host, port, db_name);
    let db = PostgresDb::new(&PostgresConfig {
        database_url: url.clone(),
        max_connections: 10,
    })
    .await
    .unwrap();
    (db, url, container)
}

fn make_deposit(
    sig: &str,
    mint: Pubkey,
    recipient: Pubkey,
    amount: u64,
) -> private_channel_indexer::storage::common::models::DbTransaction {
    DbTransactionBuilder::new(sig.to_string(), 1, mint.to_string(), amount)
        .initiator(recipient.to_string())
        .recipient(recipient.to_string())
        .transaction_type(TransactionType::Deposit)
        .build()
}

fn make_withdrawal(
    sig: &str,
    nonce: i64,
) -> private_channel_indexer::storage::common::models::DbTransaction {
    let mint = Pubkey::new_unique().to_string();
    let recipient = Pubkey::new_unique().to_string();
    let mut tx = DbTransactionBuilder::new(sig.to_string(), 1, mint, 10_000u64)
        .initiator(recipient.clone())
        .recipient(recipient)
        .transaction_type(TransactionType::Withdrawal)
        .build();
    tx.withdrawal_nonce = Some(nonce);
    tx
}

/// Insert + flip to `processing` + backdate `updated_at` past the trigger.
async fn seed_backdated_processing(
    pool: &sqlx::PgPool,
    tx_id: i64,
    age: ChronoDuration,
) -> chrono::DateTime<Utc> {
    sqlx::query("UPDATE transactions SET status = 'processing'::transaction_status WHERE id = $1")
        .bind(tx_id)
        .execute(pool)
        .await
        .unwrap();

    let backdated = Utc::now() - age;
    sqlx::query("ALTER TABLE transactions DISABLE TRIGGER update_transactions_updated_at")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("UPDATE transactions SET updated_at = $1 WHERE id = $2")
        .bind(backdated)
        .bind(tx_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE transactions ENABLE TRIGGER update_transactions_updated_at")
        .execute(pool)
        .await
        .unwrap();
    backdated
}

async fn status_of(pool: &sqlx::PgPool, id: i64) -> String {
    sqlx::query_scalar::<_, String>("SELECT status::text FROM transactions WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn counterpart_sig_of(pool: &sqlx::PgPool, id: i64) -> Option<String> {
    sqlx::query_scalar::<_, Option<String>>(
        "SELECT counterpart_signature FROM transactions WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn updated_at_of(pool: &sqlx::PgPool, id: i64) -> chrono::DateTime<Utc> {
    sqlx::query_scalar("SELECT updated_at FROM transactions WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn test_client(url: String) -> RpcClientWithRetry {
    RpcClientWithRetry::with_retry_config(
        url,
        RetryConfig {
            max_attempts: 2,
            base_delay: Duration::from_millis(5),
            max_delay: Duration::from_millis(50),
        },
        CommitmentConfig::confirmed(),
    )
}

// IT-1 / IT-D1: deposit whose persisted broadcast signature finalized to Completed,
// recovered from the durable signature with no double-mint (no sendTransaction).

#[tokio::test(flavor = "multi_thread")]
async fn it1_deposit_landed_promoted_to_completed() {
    let (db, url, _container) = start_pg("it1_landed").await;
    let storage = Arc::new(Storage::Postgres(db.clone()));
    storage.init_schema().await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let mint = Pubkey::new_unique();
    let recipient = Pubkey::new_unique();
    let tx = make_deposit(
        &Signature::new_unique().to_string(),
        mint,
        recipient,
        12_345,
    );
    let tx_id = db.insert_transaction_internal(&tx).await.unwrap();
    seed_backdated_processing(&pool, tx_id, ChronoDuration::minutes(10)).await;

    // The mint persisted this signature write-ahead before broadcast; it then landed.
    let landed_sig = Signature::new_unique();
    db.insert_release_signature_internal(tx_id, landed_sig.to_string(), 100)
        .await
        .unwrap();

    let mock = MockRpcServer::start().await;
    mock.enqueue(
        "getSignatureStatuses",
        Reply::result(json!({
            "context": {"slot": 200},
            "value": [{
                "slot": 100,
                "confirmations": null,
                "err": null,
                "status": {"Ok": null},
                "confirmationStatus": "finalized"
            }]
        })),
    );
    let client = test_client(mock.url());
    let (storage_tx, _storage_rx) = mpsc::channel::<TransactionStatusUpdate>(8);

    let metric_before = snapshot_recovered("escrow", "completed", "deposit");

    test_hooks::run_recovery_once(&storage, &client, ProgramType::Escrow, &storage_tx)
        .await
        .unwrap();

    assert_eq!(status_of(&pool, tx_id).await, "completed");
    assert_eq!(
        counterpart_sig_of(&pool, tx_id).await,
        Some(landed_sig.to_string())
    );
    // Recovery never re-mints a landed deposit (no double-mint).
    assert_eq!(mock.call_count("sendTransaction"), 0);
    assert_recovered_increment("escrow", "completed", "deposit", metric_before, "IT-1");
    mock.shutdown().await;
}

// IT-2 / IT-D2: deposit with no persisted signature, provably never broadcast,
// demoted to Pending for a safe re-mint, consulting no RPC.

#[tokio::test(flavor = "multi_thread")]
async fn it2_deposit_not_landed_demoted_to_pending() {
    let (db, url, _container) = start_pg("it2_demote").await;
    let storage = Arc::new(Storage::Postgres(db.clone()));
    storage.init_schema().await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let mint = Pubkey::new_unique();
    let recipient = Pubkey::new_unique();
    let tx = make_deposit(&Signature::new_unique().to_string(), mint, recipient, 100);
    let tx_id = db.insert_transaction_internal(&tx).await.unwrap();
    seed_backdated_processing(&pool, tx_id, ChronoDuration::minutes(10)).await;

    // No persisted signature and no RPC mocks: empty-sigs demotes without any RPC call.
    let mock = MockRpcServer::start().await;
    let client = test_client(mock.url());
    let (storage_tx, _storage_rx) = mpsc::channel::<TransactionStatusUpdate>(8);

    let metric_before = snapshot_recovered("escrow", "requeued", "deposit");

    test_hooks::run_recovery_once(&storage, &client, ProgramType::Escrow, &storage_tx)
        .await
        .unwrap();

    assert_eq!(status_of(&pool, tx_id).await, "pending");
    assert_eq!(
        mock.call_count("getSignatureStatuses"),
        0,
        "empty-sigs demote must not consult the RPC"
    );
    // Live fetcher picks it up on the next tick (out of scope here).
    assert_eq!(mock.call_count("sendTransaction"), 0);
    assert_recovered_increment("escrow", "requeued", "deposit", metric_before, "IT-2");
    mock.shutdown().await;
}

// IT-2b: deposit that WAS broadcast (persisted signature present) but whose mint is
// provably dead (null status, blockhash expired) is demoted for a safe re-mint. Unlike
// IT-2 (no signature, no RPC), this exercises the RPC finality classification driving
// the re-mint decision, the case-(B)-dead double-mint boundary for deposits.

#[tokio::test(flavor = "multi_thread")]
async fn it2b_deposit_dead_signature_demoted() {
    let (db, url, _container) = start_pg("it2b_dep_dead").await;
    let storage = Arc::new(Storage::Postgres(db.clone()));
    storage.init_schema().await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let mint = Pubkey::new_unique();
    let recipient = Pubkey::new_unique();
    let tx = make_deposit(&Signature::new_unique().to_string(), mint, recipient, 100);
    let tx_id = db.insert_transaction_internal(&tx).await.unwrap();
    seed_backdated_processing(&pool, tx_id, ChronoDuration::minutes(10)).await;
    // Persisted write-ahead before broadcast; the mint never landed and the blockhash expired.
    db.insert_release_signature_internal(tx_id, Signature::new_unique().to_string(), 100)
        .await
        .unwrap();

    let mock = MockRpcServer::start().await;
    // Status null + current height (1000) > lvbh (100) → expired/dead.
    mock.enqueue(
        "getSignatureStatuses",
        Reply::result(json!({"context": {"slot": 200}, "value": [null]})),
    );
    mock.enqueue("getBlockHeight", Reply::result(json!(1000)));
    // Ledger floor 0 covers the attempt window, so the expired absence is proven dead, not uncertain.
    mock.enqueue("getFirstAvailableBlock", Reply::result(json!(0)));
    let client = test_client(mock.url());
    let (storage_tx, _rx) = mpsc::channel::<TransactionStatusUpdate>(8);

    let metric_before = snapshot_recovered("escrow", "requeued", "deposit");

    test_hooks::run_recovery_once(&storage, &client, ProgramType::Escrow, &storage_tx)
        .await
        .unwrap();

    assert_eq!(status_of(&pool, tx_id).await, "pending");
    // Recovery classifies the dead signature but never re-mints itself (the fetcher does).
    assert_eq!(mock.call_count("sendTransaction"), 0);
    assert_recovered_increment("escrow", "requeued", "deposit", metric_before, "IT-2b");
    mock.shutdown().await;
}

// IT-3: withdrawal whose recorded release signature is dead (null status, blockhash expired) → demote.

#[tokio::test(flavor = "multi_thread")]
async fn it3_withdrawal_dead_signature_demoted() {
    let (db, url, _container) = start_pg("it3_wd_demote").await;
    let storage = Arc::new(Storage::Postgres(db.clone()));
    storage.init_schema().await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let tx = make_withdrawal(&Signature::new_unique().to_string(), 7);
    let tx_id = db.insert_transaction_internal(&tx).await.unwrap();
    seed_backdated_processing(&pool, tx_id, ChronoDuration::minutes(10)).await;
    db.insert_release_signature_internal(tx_id, Signature::new_unique().to_string(), 100)
        .await
        .unwrap();

    let mock = MockRpcServer::start().await;
    // Status null + current height (1000) > lvbh (100) → expired/dead.
    mock.enqueue(
        "getSignatureStatuses",
        Reply::result(json!({"context": {"slot": 200}, "value": [null]})),
    );
    mock.enqueue("getBlockHeight", Reply::result(json!(1000)));
    // Ledger floor 0 covers the attempt window, so the expired absence is proven dead, not uncertain.
    mock.enqueue("getFirstAvailableBlock", Reply::result(json!(0)));
    let client = test_client(mock.url());
    let (storage_tx, _rx) = mpsc::channel::<TransactionStatusUpdate>(8);

    let metric_before = snapshot_recovered("withdraw", "requeued", "withdrawal");

    test_hooks::run_recovery_once(&storage, &client, ProgramType::Withdraw, &storage_tx)
        .await
        .unwrap();

    assert_eq!(status_of(&pool, tx_id).await, "pending");
    let fresh = updated_at_of(&pool, tx_id).await;
    assert!(
        fresh > Utc::now() - ChronoDuration::seconds(5),
        "updated_at should be fresh"
    );
    assert_eq!(mock.call_count("sendTransaction"), 0);
    assert_recovered_increment("withdraw", "requeued", "withdrawal", metric_before, "IT-3");
    mock.shutdown().await;
}

// IT-4: withdrawal whose recorded release signature finalized → Completed, no re-send.

#[tokio::test(flavor = "multi_thread")]
async fn it4_withdrawal_landed_signature_completed_no_resend() {
    let (db, url, _container) = start_pg("it4_landed").await;
    let storage = Arc::new(Storage::Postgres(db.clone()));
    storage.init_schema().await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let tx = make_withdrawal(&Signature::new_unique().to_string(), 1);
    let tx_id = db.insert_transaction_internal(&tx).await.unwrap();
    seed_backdated_processing(&pool, tx_id, ChronoDuration::minutes(10)).await;
    let landed_sig = Signature::new_unique();
    db.insert_release_signature_internal(tx_id, landed_sig.to_string(), 100)
        .await
        .unwrap();

    let mock = MockRpcServer::start().await;
    mock.enqueue(
        "getSignatureStatuses",
        Reply::result(json!({
            "context": {"slot": 200},
            "value": [{
                "slot": 100,
                "confirmations": null,
                "err": null,
                "status": {"Ok": null},
                "confirmationStatus": "finalized"
            }]
        })),
    );
    let client = test_client(mock.url());
    let (storage_tx, _rx) = mpsc::channel::<TransactionStatusUpdate>(8);

    let metric_before = snapshot_recovered("withdraw", "completed", "withdrawal");

    test_hooks::run_recovery_once(&storage, &client, ProgramType::Withdraw, &storage_tx)
        .await
        .unwrap();

    assert_eq!(status_of(&pool, tx_id).await, "completed");
    assert_eq!(
        counterpart_sig_of(&pool, tx_id).await,
        Some(landed_sig.to_string())
    );
    assert_eq!(mock.call_count("sendTransaction"), 0);
    assert_recovered_increment("withdraw", "completed", "withdrawal", metric_before, "IT-4");
    mock.shutdown().await;
}

// IT-4b: withdrawal whose recorded signature is still live → left in Processing (no CAS write).

#[tokio::test(flavor = "multi_thread")]
async fn it4b_withdrawal_live_signature_left_processing() {
    let (db, url, _container) = start_pg("it4b_live").await;
    let storage = Arc::new(Storage::Postgres(db.clone()));
    storage.init_schema().await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let tx = make_withdrawal(&Signature::new_unique().to_string(), 2);
    let tx_id = db.insert_transaction_internal(&tx).await.unwrap();
    let _captured = seed_backdated_processing(&pool, tx_id, ChronoDuration::minutes(10)).await;
    db.insert_release_signature_internal(tx_id, Signature::new_unique().to_string(), 1000)
        .await
        .unwrap();

    let mock = MockRpcServer::start().await;
    // Status null + current height (50) <= lvbh (1000) → still live.
    mock.enqueue(
        "getSignatureStatuses",
        Reply::result(json!({"context": {"slot": 200}, "value": [null]})),
    );
    mock.enqueue("getBlockHeight", Reply::result(json!(50)));
    let client = test_client(mock.url());
    let (storage_tx, _rx) = mpsc::channel::<TransactionStatusUpdate>(8);

    test_hooks::run_recovery_once(&storage, &client, ProgramType::Withdraw, &storage_tx)
        .await
        .unwrap();

    assert_eq!(
        status_of(&pool, tx_id).await,
        "processing",
        "live signature must leave the row in Processing for the next sweep"
    );
    // No CAS write → updated_at stays backdated, not refreshed to "now".
    assert!(
        updated_at_of(&pool, tx_id).await < Utc::now() - ChronoDuration::minutes(5),
        "no CAS write means updated_at must stay backdated, not refreshed"
    );
    assert_eq!(mock.call_count("sendTransaction"), 0);
    mock.shutdown().await;
}

// IT-4c: withdrawal with no recorded signatures → quarantine (can't verify, double-payout risk).

#[tokio::test(flavor = "multi_thread")]
async fn it4c_withdrawal_no_signatures_quarantined() {
    let (db, url, _container) = start_pg("it4c_no_sigs").await;
    let storage = Arc::new(Storage::Postgres(db.clone()));
    storage.init_schema().await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let tx = make_withdrawal(&Signature::new_unique().to_string(), 3);
    let tx_id = db.insert_transaction_internal(&tx).await.unwrap();
    seed_backdated_processing(&pool, tx_id, ChronoDuration::minutes(10)).await;

    let mock = MockRpcServer::start().await;
    let client = test_client(mock.url());
    let (storage_tx, mut storage_rx) = mpsc::channel::<TransactionStatusUpdate>(8);

    let metric_before = snapshot_recovered("withdraw", "quarantined", "withdrawal");

    test_hooks::run_recovery_once(&storage, &client, ProgramType::Withdraw, &storage_tx)
        .await
        .unwrap();

    assert_eq!(status_of(&pool, tx_id).await, "manual_review");
    // No RPC needed — empty signature set short-circuits before classification.
    assert_eq!(mock.call_count("getSignatureStatuses"), 0);
    assert_eq!(mock.call_count("sendTransaction"), 0);
    let update = storage_rx
        .try_recv()
        .expect("manual_review update should be sent");
    let err = update.error_message.as_deref().unwrap_or("");
    assert!(
        err.contains("no broadcast signatures recorded"),
        "reason: {err}"
    );
    assert_recovered_increment(
        "withdraw",
        "quarantined",
        "withdrawal",
        metric_before,
        "IT-4c",
    );
    mock.shutdown().await;
}

// IT-4d: RPC uncertainty during classification → quarantine, never demote.

#[tokio::test(flavor = "multi_thread")]
async fn it4d_withdrawal_rpc_uncertain_quarantined() {
    let (db, url, _container) = start_pg("it4d_uncertain").await;
    let storage = Arc::new(Storage::Postgres(db.clone()));
    storage.init_schema().await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let tx = make_withdrawal(&Signature::new_unique().to_string(), 4);
    let tx_id = db.insert_transaction_internal(&tx).await.unwrap();
    seed_backdated_processing(&pool, tx_id, ChronoDuration::minutes(10)).await;
    db.insert_release_signature_internal(tx_id, Signature::new_unique().to_string(), 100)
        .await
        .unwrap();

    let mock = MockRpcServer::start().await;
    // getSignatureStatuses fails on every retry → Uncertain.
    mock.enqueue_sequence(
        "getSignatureStatuses",
        vec![
            Reply::error(-32000, "internal"),
            Reply::error(-32000, "internal"),
            Reply::error(-32000, "internal"),
        ],
    );
    let client = test_client(mock.url());
    let (storage_tx, mut storage_rx) = mpsc::channel::<TransactionStatusUpdate>(8);

    let metric_before = snapshot_recovered("withdraw", "quarantined", "withdrawal");

    test_hooks::run_recovery_once(&storage, &client, ProgramType::Withdraw, &storage_tx)
        .await
        .unwrap();

    assert_eq!(
        status_of(&pool, tx_id).await,
        "manual_review",
        "RPC uncertainty must quarantine, never silently demote"
    );
    assert_eq!(mock.call_count("sendTransaction"), 0);
    let update = storage_rx
        .try_recv()
        .expect("manual_review update should be sent");
    let err = update.error_message.as_deref().unwrap_or("");
    assert!(
        err.contains("could not verify release landed"),
        "reason: {err}"
    );
    assert_recovered_increment(
        "withdraw",
        "quarantined",
        "withdrawal",
        metric_before,
        "IT-4d",
    );
    mock.shutdown().await;
}

// IT-4e: GC backstop reclaims release sigs whose parent left Processing.

#[tokio::test(flavor = "multi_thread")]
async fn it4e_gc_reclaims_non_processing_release_sigs() {
    let (db, url, _container) = start_pg("it4e_gc").await;
    let storage = Arc::new(Storage::Postgres(db.clone()));
    storage.init_schema().await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    // One processing withdrawal (sig retained) and one completed (sig GC'd).
    let proc = make_withdrawal(&Signature::new_unique().to_string(), 10);
    let proc_id = db.insert_transaction_internal(&proc).await.unwrap();
    let done = make_withdrawal(&Signature::new_unique().to_string(), 11);
    let done_id = db.insert_transaction_internal(&done).await.unwrap();
    sqlx::query("UPDATE transactions SET status = 'processing'::transaction_status WHERE id = $1")
        .bind(proc_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE transactions SET status = 'completed'::transaction_status WHERE id = $1")
        .bind(done_id)
        .execute(&pool)
        .await
        .unwrap();
    db.insert_release_signature_internal(proc_id, Signature::new_unique().to_string(), 1)
        .await
        .unwrap();
    db.insert_release_signature_internal(done_id, Signature::new_unique().to_string(), 2)
        .await
        .unwrap();

    let mock = MockRpcServer::start().await;
    let client = test_client(mock.url());
    let (storage_tx, _rx) = mpsc::channel::<TransactionStatusUpdate>(8);

    // recover_once runs gc_stale_release_signatures at the top of the sweep.
    test_hooks::run_recovery_once(&storage, &client, ProgramType::Withdraw, &storage_tx)
        .await
        .unwrap();

    let remaining_done: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pending_release_signatures WHERE transaction_id = $1",
    )
    .bind(done_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(remaining_done, 0, "completed txn's sig must be GC'd");
    let remaining_proc: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pending_release_signatures WHERE transaction_id = $1",
    )
    .bind(proc_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(remaining_proc, 1, "processing txn's sig must be retained");
    mock.shutdown().await;
}

// IT-5 / IT-D5: deposit with a persisted signature but an RPC that cannot classify it.
// ManualReview (never a silent demote, which would risk a double-mint).

#[tokio::test(flavor = "multi_thread")]
async fn it5_rpc_failure_deposit_quarantines_to_manual_review() {
    let (db, url, _container) = start_pg("it5_rpc_down").await;
    let storage = Arc::new(Storage::Postgres(db.clone()));
    storage.init_schema().await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let mint = Pubkey::new_unique();
    let recipient = Pubkey::new_unique();
    let tx = make_deposit(&Signature::new_unique().to_string(), mint, recipient, 500);
    let tx_id = db.insert_transaction_internal(&tx).await.unwrap();
    seed_backdated_processing(&pool, tx_id, ChronoDuration::minutes(10)).await;
    db.insert_release_signature_internal(tx_id, Signature::new_unique().to_string(), 100)
        .await
        .unwrap();

    let mock = MockRpcServer::start().await;
    // The classifier's status RPC errors every attempt, so Uncertain, so quarantine.
    mock.enqueue_sequence(
        "getSignatureStatuses",
        vec![
            Reply::error(-32000, "internal"),
            Reply::error(-32000, "internal"),
            Reply::error(-32000, "internal"),
        ],
    );
    let client = test_client(mock.url());
    let (storage_tx, mut storage_rx) = mpsc::channel::<TransactionStatusUpdate>(8);

    let metric_before = snapshot_recovered("escrow", "quarantined", "deposit");

    test_hooks::run_recovery_once(&storage, &client, ProgramType::Escrow, &storage_tx)
        .await
        .unwrap();

    assert_eq!(
        status_of(&pool, tx_id).await,
        "manual_review",
        "RPC failure must NOT silently demote — fail-loud is the contract"
    );
    let update = storage_rx
        .try_recv()
        .expect("manual_review update should be sent");
    assert_eq!(update.transaction_id, tx_id);
    let err = update.error_message.as_deref().unwrap_or("");
    assert!(
        err.contains("could not verify mint landed"),
        "reason should match runbook substring: {err}"
    );
    assert_recovered_increment("escrow", "quarantined", "deposit", metric_before, "IT-5");
    mock.shutdown().await;
}

// IT-6: a malformed persisted signature is uncertainty (never read as "dead"),
// quarantine via the shared load_pending_sigs path, with no RPC consulted.

#[tokio::test(flavor = "multi_thread")]
async fn it6_malformed_stored_sig_quarantines_deposit() {
    let (db, url, _container) = start_pg("it6_malformed_sig").await;
    let storage = Arc::new(Storage::Postgres(db.clone()));
    storage.init_schema().await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let mint = Pubkey::new_unique();
    let recipient = Pubkey::new_unique();
    let tx = make_deposit(&Signature::new_unique().to_string(), mint, recipient, 700);
    let tx_id = db.insert_transaction_internal(&tx).await.unwrap();
    seed_backdated_processing(&pool, tx_id, ChronoDuration::minutes(10)).await;
    db.insert_release_signature_internal(tx_id, "not-a-valid-signature".to_string(), 100)
        .await
        .unwrap();

    let mock = MockRpcServer::start().await;
    let client = test_client(mock.url());
    let (storage_tx, mut storage_rx) = mpsc::channel::<TransactionStatusUpdate>(8);

    let metric_before = snapshot_recovered("escrow", "quarantined", "deposit");

    test_hooks::run_recovery_once(&storage, &client, ProgramType::Escrow, &storage_tx)
        .await
        .unwrap();

    assert_eq!(
        mock.call_count("getSignatureStatuses"),
        0,
        "a malformed stored signature must quarantine before any RPC"
    );
    assert_eq!(
        status_of(&pool, tx_id).await,
        "manual_review",
        "malformed signature is uncertainty so quarantine, never silent demote"
    );
    let update = storage_rx
        .try_recv()
        .expect("manual_review update should be sent");
    assert_eq!(update.transaction_id, tx_id);
    let err = update.error_message.as_deref().unwrap_or("");
    assert!(
        err.contains("malformed stored release signature"),
        "reason should name the malformed signature: {err}"
    );
    assert_recovered_increment("escrow", "quarantined", "deposit", metric_before, "IT-6");
    mock.shutdown().await;
}

// IT-7: fresh row is untouched (no RPC, no DB write).

#[tokio::test(flavor = "multi_thread")]
async fn it7_fresh_processing_row_untouched() {
    let (db, url, _container) = start_pg("it7_fresh").await;
    let storage = Arc::new(Storage::Postgres(db.clone()));
    storage.init_schema().await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let mint = Pubkey::new_unique();
    let recipient = Pubkey::new_unique();
    let tx = make_deposit(&Signature::new_unique().to_string(), mint, recipient, 100);
    let tx_id = db.insert_transaction_internal(&tx).await.unwrap();
    // Flip to processing without backdating — updated_at is "now".
    sqlx::query("UPDATE transactions SET status = 'processing'::transaction_status WHERE id = $1")
        .bind(tx_id)
        .execute(&pool)
        .await
        .unwrap();
    let pre_updated = updated_at_of(&pool, tx_id).await;

    let mock = MockRpcServer::start().await;
    let client = test_client(mock.url());
    let (storage_tx, _rx) = mpsc::channel::<TransactionStatusUpdate>(8);

    test_hooks::run_recovery_once(&storage, &client, ProgramType::Escrow, &storage_tx)
        .await
        .unwrap();

    assert_eq!(
        status_of(&pool, tx_id).await,
        "processing",
        "fresh row must not be picked up by recovery"
    );
    assert_eq!(
        updated_at_of(&pool, tx_id).await,
        pre_updated,
        "fresh row's updated_at must not change"
    );
    for method in &["getSignaturesForAddress", "getTransaction"] {
        assert_eq!(
            mock.call_count(method),
            0,
            "{method} should have 0 calls for fresh row"
        );
    }
    mock.shutdown().await;
}

// IT-8: conditional write is a no-op if the row moved between SELECT and write.

#[tokio::test(flavor = "multi_thread")]
async fn it8_conditional_write_noops_when_row_moved() {
    let (db, url, _container) = start_pg("it8_cond").await;
    let storage = Arc::new(Storage::Postgres(db.clone()));
    storage.init_schema().await.unwrap();

    let mint = Pubkey::new_unique();
    let recipient = Pubkey::new_unique();
    let tx = make_deposit(&Signature::new_unique().to_string(), mint, recipient, 100);
    let tx_id = db.insert_transaction_internal(&tx).await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    let _captured = seed_backdated_processing(&pool, tx_id, ChronoDuration::minutes(10)).await;

    // Race: row already moved off Processing → try_requeue returns false.
    sqlx::query("UPDATE transactions SET status = 'completed'::transaction_status WHERE id = $1")
        .bind(tx_id)
        .execute(&pool)
        .await
        .unwrap();

    // Call the conditional write directly with the original captured timestamp.
    let moved = storage
        .try_requeue_processing(tx_id, _captured)
        .await
        .unwrap();
    assert!(
        !moved,
        "conditional write must no-op when row moved off Processing"
    );
    assert_eq!(
        status_of(&pool, tx_id).await,
        "completed",
        "row must remain at the new status"
    );
}

// IT-9: lagging terminal write cannot stomp a recovery demote.

#[tokio::test(flavor = "multi_thread")]
async fn it9_lagging_terminal_write_no_ops_after_recovery_demote() {
    let (db, url, _container) = start_pg("it9_lagging").await;
    let storage = Arc::new(Storage::Postgres(db.clone()));
    storage.init_schema().await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let mint = Pubkey::new_unique();
    let recipient = Pubkey::new_unique();
    let tx = make_deposit(&Signature::new_unique().to_string(), mint, recipient, 100);
    let tx_id = db.insert_transaction_internal(&tx).await.unwrap();
    seed_backdated_processing(&pool, tx_id, ChronoDuration::minutes(10)).await;

    // No persisted signature, so demote with no RPC call.
    let mock = MockRpcServer::start().await;
    let client = test_client(mock.url());
    let (storage_tx, _rx) = mpsc::channel::<TransactionStatusUpdate>(8);

    test_hooks::run_recovery_once(&storage, &client, ProgramType::Escrow, &storage_tx)
        .await
        .unwrap();
    assert_eq!(status_of(&pool, tx_id).await, "pending");

    // Lagging in-flight write from dead operator — must no-op.
    db.update_transaction_status_internal(
        tx_id,
        private_channel_indexer::storage::common::models::TransactionStatus::Completed,
        Some("lagging-sig".to_string()),
        Utc::now(),
    )
    .await
    .unwrap();
    assert_eq!(
        status_of(&pool, tx_id).await,
        "pending",
        "tightened terminal write must NOT overwrite a recovery demote"
    );
    assert_eq!(
        counterpart_sig_of(&pool, tx_id).await,
        None,
        "lagging sig must NOT be persisted"
    );
    mock.shutdown().await;
}

// IT-10: 250-row backlog drained across multiple ticks.

#[tokio::test(flavor = "multi_thread")]
async fn it10_backlog_batched_across_ticks() {
    let (db, url, _container) = start_pg("it10_batched").await;
    let storage = Arc::new(Storage::Postgres(db.clone()));
    storage.init_schema().await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let mut ids: Vec<i64> = Vec::with_capacity(250);
    for _ in 0..250 {
        let mint = Pubkey::new_unique();
        let recipient = Pubkey::new_unique();
        let tx = make_deposit(&Signature::new_unique().to_string(), mint, recipient, 100);
        let id = db.insert_transaction_internal(&tx).await.unwrap();
        ids.push(id);
    }
    // Bulk: flip all to processing then backdate once.
    sqlx::query(
        "UPDATE transactions SET status = 'processing'::transaction_status WHERE id = ANY($1)",
    )
    .bind(&ids)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("ALTER TABLE transactions DISABLE TRIGGER update_transactions_updated_at")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE transactions SET updated_at = $1 WHERE id = ANY($2)")
        .bind(Utc::now() - ChronoDuration::minutes(10))
        .bind(&ids)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE transactions ENABLE TRIGGER update_transactions_updated_at")
        .execute(&pool)
        .await
        .unwrap();

    // No persisted signatures, so demote-all path, with no RPC consulted.
    let mock = MockRpcServer::start().await;
    let client = test_client(mock.url());
    let (storage_tx, _rx) = mpsc::channel::<TransactionStatusUpdate>(8);

    // Tick 1: should heal exactly RECOVERY_BATCH_LIMIT (100) rows.
    let t0 = std::time::Instant::now();
    test_hooks::run_recovery_once(&storage, &client, ProgramType::Escrow, &storage_tx)
        .await
        .unwrap();
    assert!(
        t0.elapsed() < Duration::from_secs(20),
        "single tick should not starve the live path"
    );
    let pending_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM transactions WHERE status = 'pending'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(pending_count, 100, "tick 1 must heal exactly the batch cap");

    // Ticks 2-3: drain the rest. Healed rows are excluded (trigger bumped updated_at).
    test_hooks::run_recovery_once(&storage, &client, ProgramType::Escrow, &storage_tx)
        .await
        .unwrap();
    test_hooks::run_recovery_once(&storage, &client, ProgramType::Escrow, &storage_tx)
        .await
        .unwrap();
    let pending_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM transactions WHERE status = 'pending'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        pending_count, 250,
        "all 250 rows must be healed across 3 ticks"
    );
    mock.shutdown().await;
}

// IT-11: PendingRemint rows are NOT touched by recovery.

#[tokio::test(flavor = "multi_thread")]
async fn it11_pending_remint_rows_untouched() {
    let (db, url, _container) = start_pg("it11_pending_remint").await;
    let storage = Arc::new(Storage::Postgres(db.clone()));
    storage.init_schema().await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let tx = make_withdrawal(&Signature::new_unique().to_string(), 42);
    let tx_id = db.insert_transaction_internal(&tx).await.unwrap();
    // Set up as pending_remint with backdated updated_at.
    sqlx::query("UPDATE transactions SET status = 'processing'::transaction_status WHERE id = $1")
        .bind(tx_id)
        .execute(&pool)
        .await
        .unwrap();
    db.set_pending_remint_internal(
        tx_id,
        vec!["fake-sig".to_string()],
        vec![1],
        Utc::now() + ChronoDuration::minutes(30),
    )
    .await
    .unwrap();

    sqlx::query("ALTER TABLE transactions DISABLE TRIGGER update_transactions_updated_at")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE transactions SET updated_at = $1 WHERE id = $2")
        .bind(Utc::now() - ChronoDuration::minutes(10))
        .bind(tx_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE transactions ENABLE TRIGGER update_transactions_updated_at")
        .execute(&pool)
        .await
        .unwrap();

    let mock = MockRpcServer::start().await;
    let client = test_client(mock.url());
    let (storage_tx, _rx) = mpsc::channel::<TransactionStatusUpdate>(8);

    test_hooks::run_recovery_once(&storage, &client, ProgramType::Withdraw, &storage_tx)
        .await
        .unwrap();

    assert_eq!(
        status_of(&pool, tx_id).await,
        "pending_remint",
        "pending_remint rows must not be touched by stuck-Processing recovery"
    );
    mock.shutdown().await;
}

// IT-12: withdrawal with NULL nonce → ManualReview (runbook reason string).

#[tokio::test(flavor = "multi_thread")]
async fn it12_withdrawal_missing_nonce_quarantines() {
    let (db, url, _container) = start_pg("it12_missing_nonce").await;
    let storage = Arc::new(Storage::Postgres(db.clone()));
    storage.init_schema().await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let tx = make_withdrawal(&Signature::new_unique().to_string(), 99);
    let tx_id = db.insert_transaction_internal(&tx).await.unwrap();
    // Force-null the nonce after insert (simulates a corrupt row).
    sqlx::query("UPDATE transactions SET withdrawal_nonce = NULL WHERE id = $1")
        .bind(tx_id)
        .execute(&pool)
        .await
        .unwrap();
    seed_backdated_processing(&pool, tx_id, ChronoDuration::minutes(10)).await;

    let mock = MockRpcServer::start().await;
    let client = test_client(mock.url());
    let (storage_tx, mut storage_rx) = mpsc::channel::<TransactionStatusUpdate>(8);

    let metric_before = snapshot_recovered("withdraw", "quarantined", "withdrawal");

    test_hooks::run_recovery_once(&storage, &client, ProgramType::Withdraw, &storage_tx)
        .await
        .unwrap();

    assert_eq!(status_of(&pool, tx_id).await, "manual_review");
    let update = storage_rx
        .try_recv()
        .expect("manual_review update should be sent");
    assert_eq!(
        update.error_message.as_deref(),
        Some("withdrawal row missing nonce")
    );
    assert_recovered_increment(
        "withdraw",
        "quarantined",
        "withdrawal",
        metric_before,
        "IT-12",
    );
    mock.shutdown().await;
}

// IT-13: a deposit that keeps coming back NotLanded is quarantined once it hits
// the requeue cap instead of looping pending→processing→pending forever.

#[tokio::test(flavor = "multi_thread")]
async fn it13_recovery_requeue_cap_quarantines_after_max() {
    let (db, url, _container) = start_pg("it13_requeue_cap").await;
    let storage = Arc::new(Storage::Postgres(db.clone()));
    storage.init_schema().await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let mint = Pubkey::new_unique();
    let recipient = Pubkey::new_unique();
    let tx = make_deposit(&Signature::new_unique().to_string(), mint, recipient, 100);
    let tx_id = db.insert_transaction_internal(&tx).await.unwrap();
    seed_backdated_processing(&pool, tx_id, ChronoDuration::minutes(10)).await;

    // Seed the durable counter to MAX_RECOVERY_REQUEUE_ATTEMPTS (= 3); the row
    // has already used its requeue budget, so the next demote is quarantined.
    sqlx::query("ALTER TABLE transactions DISABLE TRIGGER update_transactions_updated_at")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE transactions SET recovery_requeue_attempts = 3 WHERE id = $1")
        .bind(tx_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE transactions ENABLE TRIGGER update_transactions_updated_at")
        .execute(&pool)
        .await
        .unwrap();

    // No persisted signatures means would Demote, but the requeue cap intercepts it.
    let mock = MockRpcServer::start().await;
    let client = test_client(mock.url());
    let (storage_tx, mut storage_rx) = mpsc::channel::<TransactionStatusUpdate>(8);

    let metric_before = snapshot_recovered("escrow", "quarantined", "deposit");

    test_hooks::run_recovery_once(&storage, &client, ProgramType::Escrow, &storage_tx)
        .await
        .unwrap();

    assert_eq!(
        status_of(&pool, tx_id).await,
        "manual_review",
        "row at the requeue cap must quarantine, not loop back to pending"
    );
    let update = storage_rx
        .try_recv()
        .expect("cap must fire the manual_review alert webhook");
    assert_eq!(update.transaction_id, tx_id);
    let err = update.error_message.as_deref().unwrap_or("");
    // Count tracks MAX_RECOVERY_REQUEUE_ATTEMPTS (= 3, see the seed above); pin it to catch an off-by-one cap.
    assert!(
        err.contains("3 recovery requeues"),
        "alert must name the requeue cap and its count: {err}"
    );
    assert_eq!(mock.call_count("sendTransaction"), 0);
    assert_recovered_increment("escrow", "quarantined", "deposit", metric_before, "IT-13");
    mock.shutdown().await;
}

// Threshold boundary: three rows at -4:59 / -5:00 / -5:01, expect the two older returned.

#[tokio::test(flavor = "multi_thread")]
async fn threshold_boundary_returns_only_strictly_older_rows() {
    let (db, url, _container) = start_pg("it_boundary").await;
    let storage = Arc::new(Storage::Postgres(db.clone()));
    storage.init_schema().await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let mut ids = Vec::new();
    for _ in 0..3 {
        let mint = Pubkey::new_unique();
        let recipient = Pubkey::new_unique();
        let tx = make_deposit(&Signature::new_unique().to_string(), mint, recipient, 1);
        ids.push(db.insert_transaction_internal(&tx).await.unwrap());
    }
    sqlx::query(
        "UPDATE transactions SET status = 'processing'::transaction_status WHERE id = ANY($1)",
    )
    .bind(&ids)
    .execute(&pool)
    .await
    .unwrap();

    let ages = [
        ChronoDuration::seconds(4 * 60 + 59),
        ChronoDuration::seconds(5 * 60),
        ChronoDuration::seconds(5 * 60 + 1),
    ];
    sqlx::query("ALTER TABLE transactions DISABLE TRIGGER update_transactions_updated_at")
        .execute(&pool)
        .await
        .unwrap();
    for (id, age) in ids.iter().zip(ages.iter()) {
        sqlx::query("UPDATE transactions SET updated_at = $1 WHERE id = $2")
            .bind(Utc::now() - *age)
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }
    sqlx::query("ALTER TABLE transactions ENABLE TRIGGER update_transactions_updated_at")
        .execute(&pool)
        .await
        .unwrap();

    let stale = db
        .get_stale_processing_transactions_internal(Duration::from_secs(5 * 60), 100)
        .await
        .unwrap();
    // 4:59 excluded; 5:00 is timing-dependent (Postgres `<` is strict).
    let returned_ids: std::collections::HashSet<i64> = stale.iter().map(|r| r.id).collect();
    assert!(
        !returned_ids.contains(&ids[0]),
        "4:59-old row must NOT be returned (younger than threshold)"
    );
    assert!(
        returned_ids.contains(&ids[2]),
        "5:01-old row MUST be returned (older than threshold)"
    );
}

// ── ownership-checked deposit claim: the double-mint invariant end-to-end ─────
//
// One escrow deposit must produce at most one channel mint even when the
// recovery worker demotes a row while a live in-memory Mint builder still
// holds it. These drive the production sender's first-fire path
// (`fire_and_store_task` via `run_fire_and_store_task`) against a real
// Postgres, so the claim CAS and recovery's demote race on the same rows.

const OWNERSHIP_LOST_REASON: &str = "deposit_ownership_lost";
const MINT_BROADCAST_METHOD: &str = "sendTransaction";

/// Count private-channel mint broadcasts so each assertion is falsifiable.
fn mint_broadcast_count(mock: &MockRpcServer) -> usize {
    mock.call_count(MINT_BROADCAST_METHOD)
}

async fn build_pg_sender_state(storage: Arc<Storage>, rpc_url: String) -> SenderState {
    sender_fixtures::ensure_admin_signer_env();
    sender_hooks::new_sender_state(
        &make_config(rpc_url, ProgramType::Escrow),
        CommitmentLevel::Confirmed,
        None,
        storage,
        1,
        1,
        None,
    )
    .expect("sender state construction against Postgres storage")
}

/// Drive one deposit first-fire builder through the production persist/claim
/// path with the given ownership token.
async fn drive_first_fire(
    state: &SenderState,
    tx_id: i64,
    token: chrono::DateTime<Utc>,
    storage_tx: &mpsc::Sender<TransactionStatusUpdate>,
) {
    sender_hooks::run_fire_and_store_task(
        state,
        make_instruction(),
        None,
        deposit_ctx(tx_id),
        RetryPolicy::None,
        ExtraErrorCheckPolicy::None,
        storage_tx,
        SendDurability::Recoverable {
            deposit_expected_updated_at: token,
        },
    )
    .await;
}

// IT1: a stale sender-owned builder whose row recovery already demoted must NOT
// broadcast. This is the exact reported bug, closed.
#[tokio::test(flavor = "multi_thread")]
async fn stale_owned_builder_does_not_double_mint() {
    let (db, url, _container) = start_pg("it_claim_stale").await;
    let storage = Arc::new(Storage::Postgres(db.clone()));
    storage.init_schema().await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let tx = make_deposit(
        &Signature::new_unique().to_string(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        100,
    );
    let tx_id = db.insert_transaction_internal(&tx).await.unwrap();
    // The row was locked at T_lock; the stale builder still carries this token.
    let t_lock = seed_backdated_processing(&pool, tx_id, ChronoDuration::minutes(10)).await;

    let mock = MockRpcServer::start().await;
    // build_and_sign needs a blockhash; the claim aborts before any broadcast.
    mock.enqueue("getLatestBlockhash", blockhash_reply());
    let recovery_client = test_client(mock.url());
    let state = build_pg_sender_state(storage.clone(), mock.url()).await;
    let (storage_tx, _rx) = mpsc::channel::<TransactionStatusUpdate>(8);

    // Recovery sees empty sigs and demotes the row to Pending (bumping updated_at).
    test_hooks::run_recovery_once(&storage, &recovery_client, ProgramType::Escrow, &storage_tx)
        .await
        .unwrap();
    assert_eq!(status_of(&pool, tx_id).await, "pending");

    let metric = OPERATOR_TRANSACTION_ERRORS.with_label_values(&["escrow", OWNERSHIP_LOST_REASON]);
    let metric_before = metric.get();

    drive_first_fire(&state, tx_id, t_lock, &storage_tx).await;

    assert_eq!(
        mint_broadcast_count(&mock),
        0,
        "a demoted row's stale builder must not broadcast a mint"
    );
    assert_eq!(
        status_of(&pool, tx_id).await,
        "pending",
        "the lost claim must leave the row untouched for its current owner"
    );
    assert!(
        db.get_release_signatures_internal(tx_id)
            .await
            .unwrap()
            .is_empty(),
        "a lost claim persists no signature"
    );
    assert!(
        metric.get() > metric_before,
        "a lost claim increments deposit_ownership_lost"
    );
    mock.shutdown().await;
}

// The mid-JIT double-mint window, closed: a first mint claims and broadcasts,
// recovery demotes the row while the JIT verdict is pending, and the JIT
// re-fire then presents the epoch of its own (now superseded) claim. The
// re-claim must lose, so nothing new is journaled or broadcast and the row
// stays with its current owner.
#[tokio::test(flavor = "multi_thread")]
async fn stale_jit_refire_does_not_double_mint() {
    let (db, url, _container) = start_pg("it_stale_jit_refire").await;
    let storage = Arc::new(Storage::Postgres(db.clone()));
    storage.init_schema().await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let tx = make_deposit(
        &Signature::new_unique().to_string(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        100,
    );
    let tx_id = db.insert_transaction_internal(&tx).await.unwrap();
    let t_lock = seed_backdated_processing(&pool, tx_id, ChronoDuration::minutes(10)).await;

    let mock = MockRpcServer::start().await;
    // First fire: build/sign, claim the row, journal one signature, broadcast.
    mock.enqueue("getLatestBlockhash", blockhash_reply());
    mock.enqueue("sendTransaction", send_transaction_echo_reply());
    let mut state = build_pg_sender_state(storage.clone(), mock.url()).await;
    let (storage_tx, _rx) = mpsc::channel::<TransactionStatusUpdate>(8);

    drive_first_fire(&state, tx_id, t_lock, &storage_tx).await;
    assert_eq!(
        mint_broadcast_count(&mock),
        1,
        "the owned first fire broadcasts exactly once"
    );
    // The row's committed post-claim updated_at is the epoch the first claim
    // returned; the JIT re-fire below carries it as its ownership token.
    let claim_epoch = updated_at_of(&pool, tx_id).await;
    assert_ne!(claim_epoch, t_lock, "the first claim advances the token");

    // Recovery demotes mid-JIT: age the row past the staleness threshold and
    // classify the journaled signature dead (null status, expired blockhash,
    // covered attempt window), so the deposit is requeued to Pending.
    seed_backdated_processing(&pool, tx_id, ChronoDuration::minutes(10)).await;
    mock.enqueue(
        "getSignatureStatuses",
        Reply::result(json!({"context": {"slot": 200}, "value": [null]})),
    );
    mock.enqueue("getBlockHeight", Reply::result(json!(1000)));
    mock.enqueue("getFirstAvailableBlock", Reply::result(json!(0)));
    let recovery_client = test_client(mock.url());
    test_hooks::run_recovery_once(&storage, &recovery_client, ProgramType::Escrow, &storage_tx)
        .await
        .unwrap();
    assert_eq!(status_of(&pool, tx_id).await, "pending");
    let sigs_after_demote = db.get_release_signatures_internal(tx_id).await.unwrap();

    // The JIT re-fire: MintNotInitialized verdict, pre-check reads an
    // admin-authority initialized mint (Retry), build/sign gets a blockhash,
    // then the re-claim runs with the stale epoch and must abort.
    let mut builder = MintToBuilder::new();
    builder.mint(Pubkey::new_unique());
    state.mint_builders.insert(tx_id, builder);
    let admin_bytes =
        pack_mint_with_authority(spl_token::solana_program::program_option::COption::Some(
            SignerUtil::admin_signer().pubkey(),
        ));
    mock.enqueue("getAccountInfo", account_info_reply_bytes(&admin_bytes));
    mock.enqueue("getLatestBlockhash", blockhash_reply());

    let metric = OPERATOR_TRANSACTION_ERRORS.with_label_values(&["escrow", OWNERSHIP_LOST_REASON]);
    let metric_before = metric.get();
    let ctx = deposit_ctx_with_lease(tx_id, claim_epoch);

    sender_hooks::handle_confirmation_result(
        &mut state,
        Ok(ConfirmationResult::MintNotInitialized),
        Signature::new_unique(),
        None,
        &ctx,
        make_instruction(),
        RetryPolicy::None,
        &ExtraErrorCheckPolicy::None,
        &storage_tx,
    )
    .await;

    assert_eq!(
        mint_broadcast_count(&mock),
        1,
        "the stale JIT re-fire must not broadcast a second mint"
    );
    assert_eq!(
        db.get_release_signatures_internal(tx_id).await.unwrap(),
        sigs_after_demote,
        "a lost JIT claim journals no new signature"
    );
    assert_eq!(
        status_of(&pool, tx_id).await,
        "pending",
        "the lost claim leaves the row to its current owner"
    );
    assert!(
        metric.get() > metric_before,
        "a lost JIT claim increments deposit_ownership_lost"
    );
    mock.shutdown().await;
}

// IT2: an owned deposit mints exactly once. The happy-path oracle proving the
// guard does not strangle a legitimate mint.
#[tokio::test(flavor = "multi_thread")]
async fn owned_deposit_mints_once() {
    let (db, url, _container) = start_pg("it_claim_owned").await;
    let storage = Arc::new(Storage::Postgres(db.clone()));
    storage.init_schema().await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let tx = make_deposit(
        &Signature::new_unique().to_string(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        100,
    );
    db.insert_transaction_internal(&tx).await.unwrap();

    // Lock the deposit the way the fetcher does and carry its true post-lock token.
    let locked = storage
        .get_and_lock_pending_transactions(TransactionType::Deposit, 100)
        .await
        .unwrap();
    let row = locked.first().expect("locked deposit");
    let tx_id = row.id;
    let token = row.updated_at;

    let mock = MockRpcServer::start().await;
    mock.enqueue("getLatestBlockhash", blockhash_reply());
    mock.enqueue("sendTransaction", send_transaction_echo_reply());
    let state = build_pg_sender_state(storage.clone(), mock.url()).await;
    let (storage_tx, _rx) = mpsc::channel::<TransactionStatusUpdate>(8);

    drive_first_fire(&state, tx_id, token, &storage_tx).await;

    assert_eq!(
        mint_broadcast_count(&mock),
        1,
        "an owned deposit must broadcast exactly one mint"
    );
    assert_eq!(
        db.get_release_signatures_internal(tx_id)
            .await
            .unwrap()
            .len(),
        1,
        "the owned claim persists exactly one write-ahead signature"
    );
    assert_eq!(
        status_of(&pool, tx_id).await,
        "processing",
        "the claim keeps the row Processing (its terminal write is status-guarded)"
    );
    assert_ne!(
        updated_at_of(&pool, tx_id).await,
        token,
        "a successful claim bumps updated_at"
    );
    mock.shutdown().await;
}

// IT3: demote then re-fetch, then drive BOTH the stale first builder and the
// second builder. Exactly one mint broadcasts across the whole sequence.
#[tokio::test(flavor = "multi_thread")]
async fn demote_then_refetch_mints_exactly_once() {
    let (db, url, _container) = start_pg("it_claim_refetch").await;
    let storage = Arc::new(Storage::Postgres(db.clone()));
    storage.init_schema().await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let tx = make_deposit(
        &Signature::new_unique().to_string(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        100,
    );
    let tx_id = db.insert_transaction_internal(&tx).await.unwrap();
    // First lock at T_lock1, held by the stale builder B1.
    let t_lock1 = seed_backdated_processing(&pool, tx_id, ChronoDuration::minutes(10)).await;

    let mock = MockRpcServer::start().await;
    // Two first-fires each build+sign (blockhash); only the owned one broadcasts.
    mock.enqueue("getLatestBlockhash", blockhash_reply());
    mock.enqueue("getLatestBlockhash", blockhash_reply());
    mock.enqueue("sendTransaction", send_transaction_echo_reply());
    let recovery_client = test_client(mock.url());
    let state = build_pg_sender_state(storage.clone(), mock.url()).await;
    let (storage_tx, _rx) = mpsc::channel::<TransactionStatusUpdate>(8);

    // Recovery demotes B1's row to Pending.
    test_hooks::run_recovery_once(&storage, &recovery_client, ProgramType::Escrow, &storage_tx)
        .await
        .unwrap();

    // A fresh fetch re-locks the row as a new incarnation B2 with token T_lock2.
    let relocked = storage
        .get_and_lock_pending_transactions(TransactionType::Deposit, 100)
        .await
        .unwrap();
    let t_lock2 = relocked
        .iter()
        .find(|r| r.id == tx_id)
        .expect("row re-locked")
        .updated_at;
    assert_ne!(t_lock1, t_lock2, "the re-lock must advance the token");

    // Drive the stale B1 first (must abort), then the owned B2 (mints once).
    drive_first_fire(&state, tx_id, t_lock1, &storage_tx).await;
    drive_first_fire(&state, tx_id, t_lock2, &storage_tx).await;

    assert_eq!(
        mint_broadcast_count(&mock),
        1,
        "across demote + re-fetch, exactly one mint broadcasts"
    );
    assert_eq!(
        db.get_release_signatures_internal(tx_id)
            .await
            .unwrap()
            .len(),
        1,
        "only the owned incarnation persists a signature"
    );
    mock.shutdown().await;
}
