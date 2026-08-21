//! Integration tests for the operator lifecycle.
//!
//! Each test starts an isolated Postgres container and a Solana test validator
//! (without Geyser), inserts pending transactions directly via the storage
//! layer, and asserts that the operator pipeline processes them correctly.
//!
//! Scenarios covered:
//! 1. Single deposit mint: operator mints channel tokens for one pending deposit.
//! 2. Issuance idempotency: duplicate deposit row does not trigger a double-mint.
//! 3. Withdrawal nonce idempotency: duplicate withdrawal row releases funds only once.
//! 4. Failure alerts: a failed withdrawal fires a webhook POST; a preflight-failed
//!    mint is left Processing for recovery rather than terminalized.
//! 5. Batch deposits: operator processes 5 deposits for distinct recipients in one sweep.
//! 6. Idle operator: no phantom records created when the DB has no pending work.
//! 7. Runtime reconciliation halt: channel supply over-issued beyond escrow custody
//!    trips the durable halt, forced-unhealthy latch, quarantine, and webhook alert.
//! 8. Sequential withdrawals: two consecutive withdrawal nonces both complete correctly.
//! 9. SMT root mismatch on startup: a poisoned local SMT state (nonce 0 completed but
//!    on-chain disagrees) must drive the next pending withdrawal out of `pending` via
//!    the fatal-error path instead of silently completing.

#[path = "helpers/mod.rs"]
mod helpers;

#[path = "setup.rs"]
mod setup;

use chrono::Utc;
use helpers::private_channel_node::start_private_channel_node;
use helpers::test_types::WAIT_TIMEOUT_SECS;
use helpers::{db, generate_mint, get_token_balance, mint_to_owner, operator_util};
use mockito::Server;
use private_channel_indexer::config::{
    OperatorConfig, PrivateChannelIndexerConfig, ProgramType, StorageType,
};
use private_channel_indexer::operator;
use private_channel_indexer::operator::reconciliation::run_reconciliation;
use private_channel_indexer::operator::{RetryConfig, RpcClientWithRetry};
use private_channel_indexer::storage::common::amount::TokenAmount;
use private_channel_indexer::storage::common::models::{
    DbMint, DbMintStatus, DbTransaction, DbTransactionBuilder, TransactionStatus,
};
use private_channel_indexer::storage::{PostgresDb, Storage, TransactionType};
use private_channel_indexer::PostgresConfig;
use private_channel_metrics::{HealthConfig, HealthState};
use setup::{TestEnvironment, TEST_ADMIN_KEYPAIR};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signature, Signer};
use std::sync::Arc;
use std::time::Duration;
use test_utils::operator_helper::same_host_fallback_url;
use test_utils::operator_helper::start_solana_to_private_channel_operator;
use test_utils::operator_helper::OperatorHandle;
use test_utils::validator_helper::start_test_validator_no_geyser;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

async fn seed_mint_status_allowed(
    storage: &Storage,
    mint_address: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    storage
        .insert_mint_statuses_batch(&[DbMintStatus {
            mint_address: mint_address.to_string(),
            status: "allowed".to_string(),
            effective_slot: 0,
            signature: format!("test-seed-{mint_address}"),
            created_at: Utc::now(),
        }])
        .await?;
    Ok(())
}

fn default_operator_config(alert_url: Option<String>) -> OperatorConfig {
    OperatorConfig {
        db_poll_interval: Duration::from_millis(500),
        batch_size: 10,
        retry_max_attempts: 15,
        retry_base_delay: Duration::from_millis(500),
        channel_buffer_size: 100,
        rpc_commitment: solana_sdk::commitment_config::CommitmentLevel::Confirmed,
        alert_webhook_url: alert_url,
        reconciliation_interval: Duration::from_secs(5 * 60),
        reconciliation_tolerance_bps: 10,
        // Escrow operators refuse to start without a reconciliation webhook; a
        // placeholder satisfies the startup gate for harnesses that don't assert
        // delivery. Tests that check delivery override this with a mock URL.
        reconciliation_webhook_url: Some("http://127.0.0.1:0/recon-test".to_string()),
        feepayer_monitor_interval: Duration::from_secs(60),
        confirmation_poll_interval_ms: 400,
    }
}

fn set_operator_env_vars(keypair: &Keypair) {
    let private_key_base58 = bs58::encode(keypair.to_bytes()).into_string();
    std::env::set_var("ADMIN_SIGNER", "memory");
    std::env::set_var("ADMIN_PRIVATE_KEY", &private_key_base58);
    std::env::set_var("OPERATOR_SIGNER", "memory");
    std::env::set_var("OPERATOR_PRIVATE_KEY", &private_key_base58);
}

async fn start_operator_with_alert(
    program_type: ProgramType,
    rpc_url: String,
    db_url: String,
    operator_keypair: Keypair,
    instance: solana_sdk::pubkey::Pubkey,
    alert_url: Option<String>,
) -> Result<OperatorHandle, Box<dyn std::error::Error>> {
    let postgres_config = PostgresConfig {
        database_url: db_url.clone(),
        max_connections: 10,
    };

    let storage = Arc::new(Storage::Postgres(PostgresDb::new(&postgres_config).await?));

    let common_config = PrivateChannelIndexerConfig {
        program_type,
        storage_type: StorageType::Postgres,
        rpc_url: rpc_url.clone(),
        // Withdraw operator requires a source chain for remints; single-validator
        // test, so point it at the same RPC. Harmless for Escrow callers.
        source_rpc_url: Some(rpc_url.clone()),
        // The withdraw operator now requires an independent, same-cluster fallback.
        // Reach the same node via a distinct host string so the URL differs while
        // the genesis hash matches. Harmless for Escrow callers (never validated).
        fallback_rpc_url: Some(same_host_fallback_url(&rpc_url)),
        postgres: postgres_config,
        escrow_instance_id: Some(instance),
    };

    let operator_config = default_operator_config(alert_url);

    set_operator_env_vars(&operator_keypair);

    let task_handle: JoinHandle<()> = tokio::spawn(async move {
        if let Err(e) = operator::run(storage, common_config, operator_config, None).await {
            tracing::error!("Operator error: {}", e);
        }
    });

    Ok(OperatorHandle {
        _handle: task_handle,
    })
}

/// Start an operator task with a fully custom [`OperatorConfig`].
///
/// Use this when a test needs to override reconciliation intervals,
/// tolerance settings, or other config values beyond the alert-only defaults
/// provided by [`start_operator_with_alert`].
#[allow(dead_code)]
async fn start_operator_with_config(
    program_type: ProgramType,
    rpc_url: String,
    db_url: String,
    operator_keypair: Keypair,
    instance: solana_sdk::pubkey::Pubkey,
    operator_config: OperatorConfig,
) -> Result<OperatorHandle, Box<dyn std::error::Error>> {
    let postgres_config = PostgresConfig {
        database_url: db_url.clone(),
        max_connections: 10,
    };

    let storage = Arc::new(Storage::Postgres(PostgresDb::new(&postgres_config).await?));

    let common_config = PrivateChannelIndexerConfig {
        program_type,
        storage_type: StorageType::Postgres,
        rpc_url: rpc_url.clone(),
        // Withdraw operator requires a source chain for remints; single-validator
        // test, so point it at the same RPC. Harmless for Escrow callers.
        source_rpc_url: Some(rpc_url.clone()),
        // The withdraw operator now requires an independent, same-cluster fallback.
        // Reach the same node via a distinct host string so the URL differs while
        // the genesis hash matches. Harmless for Escrow callers (never validated).
        fallback_rpc_url: Some(same_host_fallback_url(&rpc_url)),
        postgres: postgres_config,
        escrow_instance_id: Some(instance),
    };

    set_operator_env_vars(&operator_keypair);

    let task_handle: JoinHandle<()> = tokio::spawn(async move {
        if let Err(e) = operator::run(storage, common_config, operator_config, None).await {
            tracing::error!("Operator error: {}", e);
        }
    });

    Ok(OperatorHandle {
        _handle: task_handle,
    })
}

async fn wait_for_transaction_status(
    pool: &sqlx::PgPool,
    signature: &str,
    expected_status: &str,
    timeout_secs: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let start = std::time::Instant::now();
    while start.elapsed().as_secs() < timeout_secs {
        if let Some(tx) = db::get_transaction(pool, signature).await? {
            if tx.status == expected_status {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err(format!(
        "Transaction {} did not reach status {} within {}s",
        signature, expected_status, timeout_secs
    )
    .into())
}

/// Poll until the row reaches one of `expected_statuses`, returning the one it
/// reached. A permanently-failed withdrawal is resolved by the deferred-remint
/// gate, which can land on either a completed remint or an escalation, so the
/// caller must accept both.
async fn wait_for_any_transaction_status(
    pool: &sqlx::PgPool,
    signature: &str,
    expected_statuses: &[&str],
    timeout_secs: u64,
) -> Result<String, Box<dyn std::error::Error>> {
    let start = std::time::Instant::now();
    while start.elapsed().as_secs() < timeout_secs {
        if let Some(tx) = db::get_transaction(pool, signature).await? {
            if expected_statuses.contains(&tx.status.as_str()) {
                return Ok(tx.status);
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err(format!(
        "Transaction {signature} did not reach any of {expected_statuses:?} within {timeout_secs}s"
    )
    .into())
}

/// Poll until the deposit's write-ahead mint signature is journaled in
/// `pending_release_signatures`. The journal is written right before broadcast,
/// so a non-empty journal proves the mint reached the send step.
async fn wait_for_release_signature_journaled(
    pool: &sqlx::PgPool,
    signature: &str,
    timeout_secs: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let start = std::time::Instant::now();
    while start.elapsed().as_secs() < timeout_secs {
        let (count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM pending_release_signatures prs \
             JOIN transactions t ON t.id = prs.transaction_id \
             WHERE t.signature = $1",
        )
        .bind(signature)
        .fetch_one(pool)
        .await?;
        if count > 0 {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err(format!(
        "deposit {} did not journal a write-ahead mint signature within {}s",
        signature, timeout_secs
    )
    .into())
}

fn make_withdrawal_transaction(
    signature: String,
    mint: String,
    recipient: String,
    amount: u64,
    nonce: i64,
) -> DbTransaction {
    let now = Utc::now();
    DbTransaction {
        id: 0,
        signature,
        trace_id: Uuid::new_v4().to_string(),
        slot: 1,
        initiator: recipient.clone(),
        recipient,
        mint,
        amount: TokenAmount(amount),
        memo: None,
        transaction_type: TransactionType::Withdrawal,
        withdrawal_nonce: Some(nonce),
        status: TransactionStatus::Pending,
        created_at: now,
        updated_at: now,
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
    }
}

/// Happy path: a single pending deposit is picked up by the Solana→PrivateChannel operator,
/// minted on the channel, and the DB row transitions to `completed` with a
/// non-null `counterpart_signature`.
///
/// Inserts one deposit directly via `storage.insert_db_transaction()` rather
/// than going through the on-chain indexer, keeping the test self-contained.
#[tokio::test(flavor = "multi_thread")]
async fn test_deposit_operator_processes_single_mint() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Operator Lifecycle: Single Deposit Mint ===");

    // 1. Start validator + Postgres, TestEnvironment::setup() (1 user)
    let (test_validator, faucet_keypair) = start_test_validator_no_geyser().await;
    let client =
        RpcClient::new_with_commitment(test_validator.rpc_url(), CommitmentConfig::confirmed());

    let pg_container = Postgres::default()
        .with_db_name("operator_lifecycle")
        .with_user("postgres")
        .with_password("password")
        .start()
        .await?;
    let pg_host = pg_container.get_host().await?;
    let pg_port = pg_container.get_host_port_ipv4(5432).await?;
    let db_url = format!(
        "postgres://postgres:password@{}:{}/operator_lifecycle",
        pg_host, pg_port
    );

    let pool = db::connect(&db_url).await?;
    let storage = Storage::Postgres(
        PostgresDb::new(&PostgresConfig {
            database_url: db_url.clone(),
            max_connections: 10,
        })
        .await?,
    );
    storage.init_schema().await?;

    let env = TestEnvironment::setup(&client, &faucet_keypair, 1, 1_000_000, None).await?;

    // deposit gate refuses to mint unless the mint was in `allowed`
    // status at the deposit's slot. This test bypasses the indexer, so no
    // `AllowMint` event is ingested, seed both rows manually to mirror
    // what `convert_to_db_models` + `finalize_and_checkpoint` would have
    // produced in production.
    storage
        .upsert_mints_batch(&[DbMint::new(
            env.mint.to_string(),
            6,
            spl_token::id().to_string(),
        )])
        .await?;
    seed_mint_status_allowed(&storage, &env.mint.to_string()).await?;

    // 2. Insert 1 pending deposit directly via storage.insert_db_transaction()
    let signature = Signature::new_unique().to_string();
    let recipient = env.users[0].pubkey().to_string();
    let amount = 50_000u64;

    let deposit_txn = DbTransactionBuilder::new(signature.clone(), 1, env.mint.to_string(), amount)
        .initiator(recipient.clone())
        .recipient(recipient)
        .transaction_type(TransactionType::Deposit)
        .build();

    storage.insert_db_transaction(&deposit_txn).await?;

    // 3. Start a PrivateChannel core node as the mint target. It reports every
    // found transaction as `finalized` instantly, so the operator's finalized
    // mint gate completes on the first poll - mirroring production, where mints
    // land on the PC node rather than slow Solana.
    let operator_keypair = Keypair::try_from(&TEST_ADMIN_KEYPAIR[..])?;
    let pc_node = start_private_channel_node(operator_keypair.pubkey()).await?;

    // rpc_url (mint target) = PC node; the escrow instance stays on Solana.
    let operator_handle = start_solana_to_private_channel_operator(
        pc_node.url.clone(),
        db_url.clone(),
        operator_keypair,
        env.instance,
    )
    .await?;

    // 4. wait_for_transaction_completion(pool, sig, 180s)
    operator_util::wait_for_transaction_completion(&pool, &signature, 180).await?;

    // 5. Assert status = "completed", counterpart_signature.is_some()
    let db_tx = db::get_transaction(&pool, &signature)
        .await?
        .expect("Transaction not found in DB");
    assert_eq!(db_tx.status, "completed");
    assert!(db_tx.counterpart_signature.is_some());

    operator_handle.shutdown().await;
    pc_node.shutdown().await;

    Ok(())
}

/// Inserts the same deposit row twice (same signature), starts the operator,
/// and asserts that the recipient receives exactly `amount` tokens — not `2 ×
/// amount`.  Verifies that the idempotency memo mechanism in `find_existing_
/// mint_signature` prevents a second on-chain mint for an already-processed
/// deposit.
#[tokio::test(flavor = "multi_thread")]
async fn test_issuance_operator_idempotent_no_double_mint() -> Result<(), Box<dyn std::error::Error>>
{
    println!("=== Operator Lifecycle: Issuance Idempotency ===");

    let (test_validator, faucet_keypair) = start_test_validator_no_geyser().await;
    let client =
        RpcClient::new_with_commitment(test_validator.rpc_url(), CommitmentConfig::confirmed());

    let pg_container = Postgres::default()
        .with_db_name("operator_idempotent")
        .with_user("postgres")
        .with_password("password")
        .start()
        .await?;
    let pg_host = pg_container.get_host().await?;
    let pg_port = pg_container.get_host_port_ipv4(5432).await?;
    let db_url = format!(
        "postgres://postgres:password@{}:{}/operator_idempotent",
        pg_host, pg_port
    );

    let pool = db::connect(&db_url).await?;
    let storage = Storage::Postgres(
        PostgresDb::new(&PostgresConfig {
            database_url: db_url.clone(),
            max_connections: 10,
        })
        .await?,
    );
    storage.init_schema().await?;

    let env = TestEnvironment::setup(&client, &faucet_keypair, 1, 1_000_000, None).await?;
    let user_pubkey = env.users[0].pubkey();

    // deposit gate requires an allowed status row for any mint we issue
    // private channel tokens for; the test bypasses the indexer so seed
    // the rows directly. See `test_deposit_operator_processes_single_mint`.
    storage
        .upsert_mints_batch(&[DbMint::new(
            env.mint.to_string(),
            6,
            spl_token::id().to_string(),
        )])
        .await?;
    seed_mint_status_allowed(&storage, &env.mint.to_string()).await?;

    let signature = Signature::new_unique().to_string();
    let recipient = user_pubkey.to_string();
    let amount = 50_000u64;

    let deposit_txn = DbTransactionBuilder::new(signature.clone(), 1, env.mint.to_string(), amount)
        .initiator(recipient.clone())
        .recipient(recipient)
        .transaction_type(TransactionType::Deposit)
        .build();
    storage.insert_db_transaction(&deposit_txn).await?;

    // Duplicate insert with same signature should not create a second mint.
    storage.insert_db_transaction(&deposit_txn).await?;

    // Mint target is the PrivateChannel node (instant finality), so balances are
    // checked there, not on the Solana validator. The recipient ATA does not
    // exist on the PC node until the first mint, so treat "before" as 0.
    let operator_keypair = Keypair::try_from(&TEST_ADMIN_KEYPAIR[..])?;
    let pc_node = start_private_channel_node(operator_keypair.pubkey()).await?;
    let pc_client = RpcClient::new(pc_node.url.clone());
    let balance_before = get_token_balance(&pc_client, &user_pubkey, &env.mint)
        .await
        .unwrap_or(0);

    let operator_handle = start_solana_to_private_channel_operator(
        pc_node.url.clone(),
        db_url.clone(),
        operator_keypair,
        env.instance,
    )
    .await?;

    operator_util::wait_for_transaction_completion(&pool, &signature, 180).await?;

    let balance_after = get_token_balance(&pc_client, &user_pubkey, &env.mint).await?;
    assert_eq!(
        balance_after,
        balance_before + amount,
        "Duplicate deposit should mint only once"
    );

    operator_handle.shutdown().await;
    pc_node.shutdown().await;
    Ok(())
}

/// Inserts a withdrawal row twice (same signature / nonce 0), starts the
/// PrivateChannel→Solana operator, and asserts the user receives `50_000` tokens — not
/// `100_000`.  Confirms that the duplicate DB row does not result in two
/// `ReleaseFunds` instructions being sent to the escrow program.
#[tokio::test(flavor = "multi_thread")]
async fn test_withdrawal_operator_prevents_double_withdrawal(
) -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Operator Lifecycle: Withdrawal Nonce Idempotency ===");

    let (test_validator, faucet_keypair) = start_test_validator_no_geyser().await;
    let client =
        RpcClient::new_with_commitment(test_validator.rpc_url(), CommitmentConfig::confirmed());

    let pg_container = Postgres::default()
        .with_db_name("operator_withdrawal")
        .with_user("postgres")
        .with_password("password")
        .start()
        .await?;
    let pg_host = pg_container.get_host().await?;
    let pg_port = pg_container.get_host_port_ipv4(5432).await?;
    let db_url = format!(
        "postgres://postgres:password@{}:{}/operator_withdrawal",
        pg_host, pg_port
    );

    let pool = db::connect(&db_url).await?;
    let storage = Storage::Postgres(
        PostgresDb::new(&PostgresConfig {
            database_url: db_url.clone(),
            max_connections: 10,
        })
        .await?,
    );
    storage.init_schema().await?;

    let env = TestEnvironment::setup(&client, &faucet_keypair, 1, 1_000_000, None).await?;
    TestEnvironment::setup_operator(&client, &faucet_keypair, env.instance).await?;

    // Seed mint metadata so the withdrawal operator can build the instruction.
    let mint_meta = DbMint::new(env.mint.to_string(), 6, spl_token::id().to_string());
    storage.upsert_mints_batch(&[mint_meta]).await?;
    seed_mint_status_allowed(&storage, &env.mint.to_string()).await?;

    // Ensure escrow ATA has funds to withdraw.
    let admin = Keypair::try_from(&TEST_ADMIN_KEYPAIR[..])?;
    mint_to_owner(&client, &admin, env.mint, env.instance, &admin, 200_000).await?;

    let user_pubkey = env.users[0].pubkey();
    let initial_balance = get_token_balance(&client, &user_pubkey, &env.mint).await?;

    let withdrawal_sig = Signature::new_unique().to_string();
    let withdrawal_tx = make_withdrawal_transaction(
        withdrawal_sig.clone(),
        env.mint.to_string(),
        user_pubkey.to_string(),
        50_000,
        0,
    );
    // Duplicate insert with same signature must not create a second withdrawal.
    storage.insert_db_transaction(&withdrawal_tx).await?;
    storage.insert_db_transaction(&withdrawal_tx).await?;

    let operator_keypair = Keypair::try_from(&TEST_ADMIN_KEYPAIR[..])?;
    let operator_handle = start_operator_with_alert(
        ProgramType::Withdraw,
        test_validator.rpc_url(),
        db_url.clone(),
        operator_keypair,
        env.instance,
        None,
    )
    .await?;

    // Use the env-aware timeout so coverage-instrumented runs (which set
    // PRIVATE_CHANNEL_TEST_WAIT_TIMEOUT_SECS=600) don't hit the 180 s ceiling that was
    // tuned for uninstrumented nextest.
    operator_util::wait_for_transaction_completion(&pool, &withdrawal_sig, *WAIT_TIMEOUT_SECS)
        .await?;

    let balance_after = get_token_balance(&client, &user_pubkey, &env.mint).await?;
    assert_eq!(
        balance_after,
        initial_balance + 50_000,
        "Duplicate withdrawal must not release funds again"
    );

    operator_handle.shutdown().await;
    Ok(())
}

/// Triggers one preflight-failing mint (wrong-authority `mint_to`) and one bad
/// withdrawal (mint not whitelisted on the instance).
///
/// Neither is terminalized by the sender on the send error, because both journal
/// their signature write-ahead: the mint is left Processing for recovery and fires
/// no alert, and the withdrawal is handed to the deferred-remint gate, which
/// resolves it once the journaled signature can no longer land. Only that final
/// withdrawal disposition fires a webhook, so the configured `alert_webhook_url`
/// receives exactly one POST via `db_transaction_writer::send_webhook_alert`.
///
/// Uses a `mockito` HTTP server as the webhook endpoint so no external service
/// is required.
#[tokio::test(flavor = "multi_thread")]
async fn test_failed_withdrawal_alerts_and_preflight_mint_defers_to_recovery(
) -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Operator Lifecycle: Alerts on Failure ===");

    let (test_validator, faucet_keypair) = start_test_validator_no_geyser().await;
    let client =
        RpcClient::new_with_commitment(test_validator.rpc_url(), CommitmentConfig::confirmed());

    let pg_container = Postgres::default()
        .with_db_name("operator_alerts")
        .with_user("postgres")
        .with_password("password")
        .start()
        .await?;
    let pg_host = pg_container.get_host().await?;
    let pg_port = pg_container.get_host_port_ipv4(5432).await?;
    let db_url = format!(
        "postgres://postgres:password@{}:{}/operator_alerts",
        pg_host, pg_port
    );

    let pool = db::connect(&db_url).await?;
    let storage = Storage::Postgres(
        PostgresDb::new(&PostgresConfig {
            database_url: db_url.clone(),
            max_connections: 10,
        })
        .await?,
    );
    storage.init_schema().await?;

    let env = TestEnvironment::setup(&client, &faucet_keypair, 1, 1_000_000, None).await?;
    TestEnvironment::setup_operator(&client, &faucet_keypair, env.instance).await?;

    let mut server = Server::new_async().await;
    // Only the withdrawal's ManualReview disposition fires a webhook; the
    // preflight-failed mint is deferred to recovery and writes no status.
    let alert_mock = server
        .mock("POST", "/")
        .match_header("content-type", "application/json")
        .with_status(200)
        .expect(1)
        .create_async()
        .await;

    // Start Solana -> PrivateChannel operator with alert URL and low retry count so bad
    // transactions fail quickly without exhausting a long wait window.
    let operator_keypair = Keypair::try_from(&TEST_ADMIN_KEYPAIR[..])?;
    let fast_fail_config = OperatorConfig {
        retry_max_attempts: 3,
        ..default_operator_config(Some(server.url()))
    };
    let solana_to_private_channel = start_operator_with_config(
        ProgramType::Escrow,
        test_validator.rpc_url(),
        db_url.clone(),
        operator_keypair,
        env.instance,
        fast_fail_config,
    )
    .await?;

    // Create a valid SPL mint with a *different* mint authority than the operator's
    // admin key. When the operator calls mint_to using the admin key, the SPL token
    // program rejects it (wrong authority), so the mint fails at preflight after the
    // write-ahead signature is persisted.
    let admin = Keypair::try_from(&TEST_ADMIN_KEYPAIR[..])?;
    let bad_authority = Keypair::new(); // NOT the operator admin — intentionally wrong
    let bad_mint = Keypair::new();
    generate_mint(&client, &admin, &bad_authority, &bad_mint).await?;

    // Register the bad mint in the mints table so the operator's pending-deposit
    // query (which joins with mints) can find and attempt to process this deposit.
    // Without this, the deposit sits in "pending" forever → test hangs.
    let bad_mint_meta = DbMint::new(
        bad_mint.pubkey().to_string(),
        6,
        spl_token::id().to_string(),
    );
    storage.upsert_mints_batch(&[bad_mint_meta]).await?;
    seed_mint_status_allowed(&storage, &bad_mint.pubkey().to_string()).await?;

    let mint_fail_sig = Signature::new_unique().to_string();
    let recipient = env.users[0].pubkey().to_string();
    let bad_deposit = DbTransactionBuilder::new(
        mint_fail_sig.clone(),
        1,
        bad_mint.pubkey().to_string(),
        10_000,
    )
    .initiator(recipient.clone())
    .recipient(recipient)
    .transaction_type(TransactionType::Deposit)
    .build();
    storage.insert_db_transaction(&bad_deposit).await?;

    // A write-ahead-persisted mint is never terminalized in the sender: the send
    // error leaves the row Processing for recovery to reconcile against the persisted
    // signature. So the mint journals its signature and stays Processing, never Failed.
    wait_for_release_signature_journaled(&pool, &mint_fail_sig, 180).await?;
    let mint_row = db::get_transaction(&pool, &mint_fail_sig)
        .await?
        .expect("the deposit row must exist");
    assert_eq!(
        mint_row.status, "processing",
        "a preflight-failed mint is deferred to recovery, not marked Failed"
    );

    // Seed a separate mint that is NOT allowed on the instance to force withdrawal failure.
    let bad_withdraw_mint = Keypair::new();
    let bad_mint_pubkey = generate_mint(&client, &admin, &admin, &bad_withdraw_mint).await?;
    let mint_meta = DbMint::new(bad_mint_pubkey.to_string(), 6, spl_token::id().to_string());
    storage.upsert_mints_batch(&[mint_meta]).await?;
    seed_mint_status_allowed(&storage, &bad_mint_pubkey.to_string()).await?;
    mint_to_owner(
        &client,
        &admin,
        bad_mint_pubkey,
        env.instance,
        &admin,
        100_000,
    )
    .await?;

    let withdrawal_sig = Signature::new_unique().to_string();
    let withdrawal_tx = make_withdrawal_transaction(
        withdrawal_sig.clone(),
        bad_mint_pubkey.to_string(),
        env.users[0].pubkey().to_string(),
        25_000,
        0,
    );
    storage.insert_db_transaction(&withdrawal_tx).await?;

    // Start PrivateChannel -> Solana operator with same alert URL and low retry count.
    let operator_keypair = Keypair::try_from(&TEST_ADMIN_KEYPAIR[..])?;
    let fast_fail_config = OperatorConfig {
        retry_max_attempts: 3,
        ..default_operator_config(Some(server.url()))
    };
    let private_channel_to_solana = start_operator_with_config(
        ProgramType::Withdraw,
        test_validator.rpc_url(),
        db_url.clone(),
        operator_keypair,
        env.instance,
        fast_fail_config,
    )
    .await?;

    // The bad withdrawal preflights with `invalid account data for instruction`
    // from the escrow program (the mint isn't whitelisted on the instance), so the
    // send fails — but only after the write-ahead journal recorded its signature.
    // The sender cannot tell a preflight rejection from an ambiguous transport
    // error, so it hands the row to the deferred-remint gate instead of
    // terminalizing it as never-broadcast.
    wait_for_transaction_status(&pool, &withdrawal_sig, "pending_remint", 180).await?;

    // Once the tip passes the journaled signature's blockhash validity the gate
    // proves it dead and remints the burned tokens; if it exhausts its deferral
    // budget first it escalates instead. Both are terminal and fire exactly one
    // webhook, which is what this test asserts. The env-aware timeout keeps
    // coverage-instrumented runs off the uninstrumented ceiling.
    let disposition = wait_for_any_transaction_status(
        &pool,
        &withdrawal_sig,
        &["failed_reminted", "manual_review"],
        *WAIT_TIMEOUT_SECS,
    )
    .await?;
    println!("bad withdrawal settled as {disposition}");

    alert_mock.assert();

    solana_to_private_channel.shutdown().await;
    private_channel_to_solana.shutdown().await;
    Ok(())
}

/// operator fetches and processes deposits for multiple distinct
/// recipients in a single sweep.
///
/// Seeds 5 pending deposits, one per user, then asserts that every deposit
/// reaches "completed" status and each recipient's token balance increases by
/// exactly the deposited amount.
#[tokio::test(flavor = "multi_thread")]
async fn test_batch_deposits_multiple_recipients() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Operator Lifecycle: Batch Deposits ===");

    const NUM_USERS: usize = 5;
    const DEPOSIT_AMOUNT: u64 = 30_000;

    let (test_validator, faucet_keypair) = start_test_validator_no_geyser().await;
    let client =
        RpcClient::new_with_commitment(test_validator.rpc_url(), CommitmentConfig::confirmed());

    let pg_container = Postgres::default()
        .with_db_name("operator_batch")
        .with_user("postgres")
        .with_password("password")
        .start()
        .await?;
    let pg_host = pg_container.get_host().await?;
    let pg_port = pg_container.get_host_port_ipv4(5432).await?;
    let db_url = format!(
        "postgres://postgres:password@{}:{}/operator_batch",
        pg_host, pg_port
    );

    let pool = db::connect(&db_url).await?;
    let storage = Storage::Postgres(
        PostgresDb::new(&PostgresConfig {
            database_url: db_url.clone(),
            max_connections: 10,
        })
        .await?,
    );
    storage.init_schema().await?;

    // Create 5 users with 0 initial balance so we can verify the exact deposit amount.
    let env = TestEnvironment::setup(&client, &faucet_keypair, NUM_USERS, 0, None).await?;

    // deposit gate requires an allowed status row for any mint we issue
    // private channel tokens for; the test bypasses the indexer so seed
    // the rows directly. See `test_deposit_operator_processes_single_mint`.
    storage
        .upsert_mints_batch(&[DbMint::new(
            env.mint.to_string(),
            6,
            spl_token::id().to_string(),
        )])
        .await?;
    seed_mint_status_allowed(&storage, &env.mint.to_string()).await?;

    // Insert one pending deposit per user, each with a unique on-chain signature.
    let mut signatures = Vec::with_capacity(NUM_USERS);
    for user in &env.users {
        let sig = Signature::new_unique().to_string();
        let recipient = user.pubkey().to_string();
        let txn = DbTransactionBuilder::new(sig.clone(), 1, env.mint.to_string(), DEPOSIT_AMOUNT)
            .initiator(recipient.clone())
            .recipient(recipient)
            .transaction_type(TransactionType::Deposit)
            .build();
        storage.insert_db_transaction(&txn).await?;
        signatures.push(sig);
    }

    // Start the Solana -> PrivateChannel operator and wait for all deposits to be processed.
    // The mint target is a PrivateChannel node (instant finality), so balances
    // are verified there rather than on the Solana validator.
    let operator_keypair = Keypair::try_from(&TEST_ADMIN_KEYPAIR[..])?;
    let pc_node = start_private_channel_node(operator_keypair.pubkey()).await?;
    let pc_client = RpcClient::new(pc_node.url.clone());
    let operator_handle = start_solana_to_private_channel_operator(
        pc_node.url.clone(),
        db_url.clone(),
        operator_keypair,
        env.instance,
    )
    .await?;

    operator_util::wait_for_operator_completion(&pool, NUM_USERS, "batch deposits").await?;

    // Every deposit must be completed with a counterpart signature, and each
    // user must hold exactly DEPOSIT_AMOUNT tokens (one mint per deposit).
    for (i, sig) in signatures.iter().enumerate() {
        let db_tx = db::get_transaction(&pool, sig)
            .await?
            .expect("Transaction not found in DB");
        assert_eq!(db_tx.status, "completed", "Deposit {i} not completed");
        assert!(
            db_tx.counterpart_signature.is_some(),
            "Deposit {i} missing counterpart signature"
        );

        let balance = get_token_balance(&pc_client, &env.users[i].pubkey(), &env.mint).await?;
        assert_eq!(
            balance, DEPOSIT_AMOUNT,
            "User {i} balance mismatch after deposit"
        );
    }

    operator_handle.shutdown().await;
    pc_node.shutdown().await;
    Ok(())
}

/// Edge case: operator must remain alive and produce no spurious records when
/// the database contains zero pending transactions.
///
/// Lets the operator idle through several polling cycles, then asserts that
/// neither completed nor failed transactions appeared (no phantom processing).
#[tokio::test(flavor = "multi_thread")]
async fn test_operator_idle_no_pending_transactions() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Operator Lifecycle: Idle with No Pending Transactions ===");

    let (test_validator, faucet_keypair) = start_test_validator_no_geyser().await;
    let client =
        RpcClient::new_with_commitment(test_validator.rpc_url(), CommitmentConfig::confirmed());

    let pg_container = Postgres::default()
        .with_db_name("operator_idle")
        .with_user("postgres")
        .with_password("password")
        .start()
        .await?;
    let pg_host = pg_container.get_host().await?;
    let pg_port = pg_container.get_host_port_ipv4(5432).await?;
    let db_url = format!(
        "postgres://postgres:password@{}:{}/operator_idle",
        pg_host, pg_port
    );

    let pool = db::connect(&db_url).await?;
    let storage = Storage::Postgres(
        PostgresDb::new(&PostgresConfig {
            database_url: db_url.clone(),
            max_connections: 10,
        })
        .await?,
    );
    storage.init_schema().await?;

    // Set up the on-chain instance with no users and no pending transactions.
    let env = TestEnvironment::setup(&client, &faucet_keypair, 0, 0, None).await?;

    let operator_keypair = Keypair::try_from(&TEST_ADMIN_KEYPAIR[..])?;
    let operator_handle = start_solana_to_private_channel_operator(
        test_validator.rpc_url(),
        db_url.clone(),
        operator_keypair,
        env.instance,
    )
    .await?;

    // Run through multiple polling cycles (db_poll_interval = 500 ms default → ~6 cycles).
    tokio::time::sleep(Duration::from_secs(3)).await;

    // The operator must not have created or mutated any records.
    let completed = db::count_transactions_by_status(&pool, "completed").await?;
    let failed = db::count_transactions_by_status(&pool, "failed").await?;
    assert_eq!(
        completed, 0,
        "Expected no completed transactions in idle run"
    );
    assert_eq!(failed, 0, "Expected no failed transactions in idle run");

    operator_handle.shutdown().await;
    Ok(())
}

/// Runtime reconciliation must fail closed when on-chain channel-token supply
/// exceeds escrow custody beyond the in-flight envelope (the custody-vs-supply
/// invariant, which no other end-to-end test exercises). Supply is over-issued to a
/// non-escrow holder so custody stays 0, then the durable halt flag, the
/// forced-unhealthy latch, and quarantine of an active withdrawal are all
/// asserted. The halt flag is polled with a timeout rather than slept on: it is
/// monotonic (once set it stays set) and the mismatch is persistent, so every
/// tick breaches and the 3-tick confirmation can never race.
#[tokio::test(flavor = "multi_thread")]
async fn test_runtime_reconciliation_halts_on_supply_over_issuance(
) -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Operator Lifecycle: Reconciliation Halts on Supply Over-Issuance ===");

    const OVER_ISSUED: u64 = 100_000;
    const WITHDRAWAL_AMOUNT: u64 = 1_000;

    let (test_validator, faucet_keypair) = start_test_validator_no_geyser().await;
    let client =
        RpcClient::new_with_commitment(test_validator.rpc_url(), CommitmentConfig::confirmed());

    let pg_container = Postgres::default()
        .with_db_name("operator_reconciliation")
        .with_user("postgres")
        .with_password("password")
        .start()
        .await?;
    let pg_host = pg_container.get_host().await?;
    let pg_port = pg_container.get_host_port_ipv4(5432).await?;
    let db_url = format!(
        "postgres://postgres:password@{}:{}/operator_reconciliation",
        pg_host, pg_port
    );

    let storage = Storage::Postgres(
        PostgresDb::new(&PostgresConfig {
            database_url: db_url.clone(),
            max_connections: 10,
        })
        .await?,
    );
    storage.init_schema().await?;

    // Escrow instance + allowed mint, escrow ATA custody 0, mint supply 0.
    let env = TestEnvironment::setup(&client, &faucet_keypair, 0, 0, None).await?;
    let mint = env.mint;
    let instance = env.instance;

    let mint_meta = DbMint::new(mint.to_string(), 6, spl_token::id().to_string());
    storage.upsert_mints_batch(&[mint_meta]).await?;
    seed_mint_status_allowed(&storage, &mint.to_string()).await?;

    // Over-issue: mint supply to a NON-escrow holder so Mint.supply rises while
    // the escrow custody stays 0. This is supply exceeding custody with no
    // backing deposit, the operator-key over-issuance the invariant must catch.
    let admin = Keypair::try_from(&TEST_ADMIN_KEYPAIR[..])
        .map_err(|e| format!("Failed to create admin keypair: {}", e))?;
    let holder = Keypair::new();
    mint_to_owner(&client, &admin, mint, holder.pubkey(), &admin, OVER_ISSUED).await?;

    // One active (pending) withdrawal so the halt's quarantine has a row to flip.
    // Its amount joins the mint's in-flight envelope but stays well under the gap.
    let wd_sig = Signature::new_unique().to_string();
    let withdrawal =
        DbTransactionBuilder::new(wd_sig.clone(), 1, mint.to_string(), WITHDRAWAL_AMOUNT)
            .initiator(Pubkey::new_unique().to_string())
            .recipient(Pubkey::new_unique().to_string())
            .transaction_type(TransactionType::Withdrawal)
            .build();
    storage.insert_db_transaction(&withdrawal).await?;

    // The halt's alert POST is the only webhook under the single-invariant design;
    // assert it fired once the halt is observed.
    let mut mock_server = Server::new_async().await;
    let recon_mock = mock_server
        .mock("POST", "/")
        .with_status(200)
        .expect_at_least(1)
        .create_async()
        .await;

    let recon_config = OperatorConfig {
        reconciliation_interval: Duration::from_millis(200),
        reconciliation_tolerance_bps: 0,
        reconciliation_webhook_url: Some(mock_server.url()),
        ..default_operator_config(None)
    };

    let rpc_client = Arc::new(RpcClientWithRetry::with_retry_config(
        test_validator.rpc_url(),
        RetryConfig::default(),
        CommitmentConfig::confirmed(),
    ));
    // Single-validator test: custody and channel-supply reads both hit the same RPC.
    let channel_rpc_client = rpc_client.clone();

    let health = HealthState::new(HealthConfig::operator());
    let health_for_task = Some(health.clone());

    let recon_storage = Arc::new(Storage::Postgres(
        PostgresDb::new(&PostgresConfig {
            database_url: db_url.clone(),
            max_connections: 5,
        })
        .await?,
    ));

    let cancellation_token = CancellationToken::new();
    let recon_token_clone = cancellation_token.clone();
    let recon_handle: JoinHandle<()> = tokio::spawn(async move {
        if let Err(e) = run_reconciliation(
            recon_storage,
            recon_config,
            rpc_client,
            channel_rpc_client,
            instance,
            health_for_task,
            recon_token_clone,
        )
        .await
        {
            tracing::error!("Reconciliation task error: {}", e);
        }
    });

    // Poll the monotonic halt flag with a deadline rather than sleeping a fixed
    // window, so a slow CI just takes a few more ticks instead of going flaky.
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let halt = loop {
        if let Some(info) = storage.is_reconciliation_halted().await? {
            break info;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "reconciliation did not halt within the timeout"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };

    cancellation_token.cancel();
    let _ = recon_handle.await;

    // The reason names the mint and cites the supply gap, proving the custody-vs-supply
    // invariant tripped the halt.
    assert!(
        halt.reason.contains(&mint.to_string()),
        "halt reason should name the mint: {}",
        halt.reason
    );
    assert!(
        halt.reason.contains("supply by"),
        "halt reason should cite the supply gap: {}",
        halt.reason
    );

    // The halt fired the webhook alert, the only alert under the single invariant.
    recon_mock.assert_async().await;

    // The forced-unhealthy latch is set so /health reports 503 for orchestration.
    assert!(
        !health.is_healthy(),
        "operator health must be forced unhealthy after a halt"
    );

    // The active withdrawal was quarantined to manual_review by the halt.
    let pool = db::connect(&db_url).await?;
    let status: String =
        sqlx::query_scalar("SELECT status::text FROM transactions WHERE signature = $1")
            .bind(&wd_sig)
            .fetch_one(&pool)
            .await?;
    assert_eq!(
        status, "manual_review",
        "active withdrawal must be quarantined when the pipeline halts"
    );

    Ok(())
}

/// (sequential SMT proofs): the withdrawal operator correctly builds and
/// submits SMT exclusion proofs for two consecutive withdrawal nonces.
///
/// The sender processes transactions sequentially; nonce 0 is inserted into the
/// local Sparse Merkle Tree first, so the exclusion proof for nonce 1 is built
/// against a tree that already contains nonce 0. Both withdrawals must complete
/// and the recipient must receive 2 × withdrawal amount.
#[tokio::test(flavor = "multi_thread")]
async fn test_sequential_withdrawals_multiple_nonces() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Operator Lifecycle: Sequential Withdrawals (Multi-Nonce) ===");

    const ESCROW_FUND: u64 = 200_000;
    const WITHDRAWAL_AMOUNT: u64 = 50_000;

    let (test_validator, faucet_keypair) = start_test_validator_no_geyser().await;
    let client =
        RpcClient::new_with_commitment(test_validator.rpc_url(), CommitmentConfig::confirmed());

    let pg_container = Postgres::default()
        .with_db_name("operator_multi_nonce")
        .with_user("postgres")
        .with_password("password")
        .start()
        .await?;
    let pg_host = pg_container.get_host().await?;
    let pg_port = pg_container.get_host_port_ipv4(5432).await?;
    let db_url = format!(
        "postgres://postgres:password@{}:{}/operator_multi_nonce",
        pg_host, pg_port
    );

    let pool = db::connect(&db_url).await?;
    let storage = Storage::Postgres(
        PostgresDb::new(&PostgresConfig {
            database_url: db_url.clone(),
            max_connections: 10,
        })
        .await?,
    );
    storage.init_schema().await?;

    // One user will receive both withdrawals.
    let env = TestEnvironment::setup(&client, &faucet_keypair, 1, 0, None).await?;
    TestEnvironment::setup_operator(&client, &faucet_keypair, env.instance).await?;

    // Register the mint so the withdrawal processor can build the instruction.
    let mint_meta = DbMint::new(env.mint.to_string(), 6, spl_token::id().to_string());
    storage.upsert_mints_batch(&[mint_meta]).await?;
    seed_mint_status_allowed(&storage, &env.mint.to_string()).await?;

    // Fund the escrow ATA with enough tokens to cover both withdrawals.
    let admin = Keypair::try_from(&TEST_ADMIN_KEYPAIR[..])?;
    mint_to_owner(&client, &admin, env.mint, env.instance, &admin, ESCROW_FUND).await?;

    let user_pubkey = env.users[0].pubkey();
    let initial_balance = get_token_balance(&client, &user_pubkey, &env.mint).await?;

    // Insert two withdrawals with sequential nonces.  The sender processes them
    // in arrival order: nonce 0 is committed to the local SMT first, then the
    // exclusion proof for nonce 1 is generated against the updated tree root.
    let sig0 = Signature::new_unique().to_string();
    let sig1 = Signature::new_unique().to_string();

    storage
        .insert_db_transaction(&make_withdrawal_transaction(
            sig0.clone(),
            env.mint.to_string(),
            user_pubkey.to_string(),
            WITHDRAWAL_AMOUNT,
            0, // nonce 0 — committed to SMT first
        ))
        .await?;
    storage
        .insert_db_transaction(&make_withdrawal_transaction(
            sig1.clone(),
            env.mint.to_string(),
            user_pubkey.to_string(),
            WITHDRAWAL_AMOUNT,
            1, // nonce 1 — proof built after nonce 0 is in the tree
        ))
        .await?;

    let operator_keypair = Keypair::try_from(&TEST_ADMIN_KEYPAIR[..])?;
    let operator_handle = start_operator_with_alert(
        ProgramType::Withdraw,
        test_validator.rpc_url(),
        db_url.clone(),
        operator_keypair,
        env.instance,
        None,
    )
    .await?;

    // Wait until both withdrawals are completed.
    operator_util::wait_for_operator_completion(&pool, 2, "sequential withdrawals").await?;

    // Both transactions must be completed with a counterpart signature.
    for (idx, sig) in [&sig0, &sig1].iter().enumerate() {
        let db_tx = db::get_transaction(&pool, sig)
            .await?
            .expect("Transaction not found in DB");
        assert_eq!(db_tx.status, "completed", "Withdrawal {idx} not completed");
        assert!(
            db_tx.counterpart_signature.is_some(),
            "Withdrawal {idx} missing counterpart signature"
        );
    }

    // The recipient must have received tokens from both withdrawals.
    let final_balance = get_token_balance(&client, &user_pubkey, &env.mint).await?;
    assert_eq!(
        final_balance,
        initial_balance + WITHDRAWAL_AMOUNT * 2,
        "User should have received both withdrawal amounts"
    );

    operator_handle.shutdown().await;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// SMT root mismatch detected when processing a withdrawal
//
// The withdrawal-side operator's `initialize_smt_state` fetches the on-chain
// SMT root from the escrow instance PDA, rebuilds the same root locally from
// completed withdrawal nonces in the DB, and refuses to proceed if the two
// don't match (see `initialize_smt_state` in `sender/state.rs`). The check
// runs *lazily* — only when the first `ReleaseFunds` transaction is about
// to be built (see `handle_transaction_builder` in `sender/transaction.rs`).
//
// Production behaviour on mismatch: the error propagates up to the
// `handle_transaction_builder` caller, which logs it, increments an error
// counter, and calls `send_fatal_error` to mark the specific withdrawal as
// failed. The operator process itself keeps running so that other
// (non-withdrawal) pipelines aren't taken down.
//
// We trigger the mismatch by pre-seeding:
//   (a) a COMPLETED withdrawal at nonce 0 — poisons the SMT state
//   (b) a PENDING withdrawal at nonce 1 — forces the SMT-init path to run
// and assert the pending withdrawal transitions to a non-pending terminal
// status (the fatal-error path inside `handle_transaction_builder`) within
// a bounded wait.
//
// Target: the `SmtRootMismatch` branch in `sender/state.rs`.
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn test_operator_aborts_on_smt_root_mismatch_at_startup(
) -> Result<(), Box<dyn std::error::Error>> {
    use test_utils::operator_helper::start_private_channel_to_solana_operator;

    println!("=== Operator Lifecycle: SMT Root Mismatch Aborts Startup ===");

    let (test_validator, faucet_keypair) = start_test_validator_no_geyser().await;
    let client =
        RpcClient::new_with_commitment(test_validator.rpc_url(), CommitmentConfig::confirmed());

    let pg_container = Postgres::default()
        .with_db_name("operator_smt_mismatch")
        .with_user("postgres")
        .with_password("password")
        .start()
        .await?;
    let pg_host = pg_container.get_host().await?;
    let pg_port = pg_container.get_host_port_ipv4(5432).await?;
    let db_url = format!(
        "postgres://postgres:password@{}:{}/operator_smt_mismatch",
        pg_host, pg_port
    );

    let pool = db::connect(&db_url).await?;
    let storage = Storage::Postgres(
        PostgresDb::new(&PostgresConfig {
            database_url: db_url.clone(),
            max_connections: 10,
        })
        .await?,
    );
    storage.init_schema().await?;

    // Fresh instance: on-chain SMT root is [0u8; 32] (empty tree).
    let env = TestEnvironment::setup(&client, &faucet_keypair, 1, 0, None).await?;
    TestEnvironment::setup_operator(&client, &faucet_keypair, env.instance).await?;
    let mint_meta = DbMint::new(env.mint.to_string(), 6, spl_token::id().to_string());
    storage.upsert_mints_batch(&[mint_meta]).await?;
    seed_mint_status_allowed(&storage, &env.mint.to_string()).await?;

    // Step 1: seed the DB with a COMPLETED withdrawal at nonce 0 — this
    // poisons the SMT state: when the operator rebuilds the SMT locally
    // it will insert nonce 0 and compute a non-zero root, while the fresh
    // on-chain instance still has the default empty root.
    let poison_sig = Signature::new_unique().to_string();
    let fake_completed = make_withdrawal_transaction(
        poison_sig.clone(),
        env.mint.to_string(),
        env.users[0].pubkey().to_string(),
        10_000,
        0, // nonce
    );
    storage.insert_db_transaction(&fake_completed).await?;
    sqlx::query(
        "UPDATE transactions SET status = 'completed'::transaction_status WHERE signature = $1",
    )
    .bind(&poison_sig)
    .execute(&pool)
    .await?;

    // Step 2: add a PENDING withdrawal at nonce 1. The boot pre-flight validates the
    // SMT root BEFORE any row is fetched, so on a mismatch the operator refuses to
    // start and this row is never processed; it must stay `pending`.
    let trigger_sig = Signature::new_unique().to_string();
    let trigger_withdrawal = make_withdrawal_transaction(
        trigger_sig.clone(),
        env.mint.to_string(),
        env.users[0].pubkey().to_string(),
        5_000,
        1, // nonce
    );
    storage.insert_db_transaction(&trigger_withdrawal).await?;

    // Start the withdrawal-side operator. Its boot pre-flight rebuilds the SMT from
    // the poisoned `completed` nonce, finds it diverges from the empty on-chain root,
    // and refuses to start (returns Err); the operator task then exits.
    let operator_keypair = Keypair::try_from(&TEST_ADMIN_KEYPAIR[..])?;
    let operator_handle = start_private_channel_to_solana_operator(
        test_validator.rpc_url(),
        test_validator.rpc_url(),
        db_url.clone(),
        operator_keypair,
        env.instance,
    )
    .await?;

    // The operator must refuse to start: the task exits without processing any row.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut exited = false;
    while std::time::Instant::now() < deadline {
        if operator_handle._handle.is_finished() {
            exited = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(
        exited,
        "operator must refuse to start (the task exits) on a startup SMT root mismatch"
    );

    // SOLA2-21: the innocent trigger withdrawal is never moved out of `pending`
    // (never `failed`, never `completed`) because the pre-flight gates before the
    // pipeline runs.
    let trigger = db::get_transaction(&pool, &trigger_sig)
        .await?
        .expect("trigger withdrawal row must exist");
    assert_eq!(
        trigger.status, "pending",
        "trigger withdrawal must stay pending on refuse-to-start; got {}",
        trigger.status
    );

    operator_handle.shutdown().await;
    Ok(())
}
