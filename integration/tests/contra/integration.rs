use anyhow::Result;
use contra_escrow_program_client::CONTRA_ESCROW_PROGRAM_ID;
use testcontainers::{ContainerAsync, ImageExt};

#[path = "./rpc/mod.rs"]
mod rpc;

#[path = "../helpers.rs"]
mod helpers;

#[path = "../setup.rs"]
mod setup;

use test_utils::indexer_helper::{start_contra_indexer, start_l1_indexer, IndexerHandle};
use test_utils::operator_helper::{
    start_contra_to_l1_operator, start_l1_to_contra_operator, OperatorHandle,
};
use test_utils::validator_helper::start_test_validator;

use {
    contra_core::nodes::node::{NodeConfig, NodeHandles, NodeMode},
    helpers::get_free_port,
    rpc::*,
    solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer},
    std::time::Duration,
    testcontainers::runners::AsyncRunner,
    testcontainers_modules::{postgres::Postgres, redis::Redis},
    tokio::sync::Mutex,
};

static SETUP_LOCK: Mutex<()> = Mutex::const_new(());
const TEST_TIMEOUT: Duration = Duration::from_secs(300);

// We store these only to keep the services alive for the duration of the test
struct KeepAlive {
    _test_validator: solana_test_validator::TestValidator,
    _contra_indexer_db: ContainerAsync<Postgres>,
    _l1_indexer_db: ContainerAsync<Postgres>,
}

struct TestContext {
    _keep_alive: KeepAlive,
    l1_to_contra_operator_handle: OperatorHandle,
    contra_to_l1_operator_handle: OperatorHandle,
    contra_indexer_handle: IndexerHandle,
    l1_indexer_handle: IndexerHandle,
    contra_handles: NodeHandles,
    contra_ctx: ContraContext,
    l1_ctx: L1Context,
}

#[tokio::test(flavor = "multi_thread")]
async fn test_with_postgres() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        // Start PostgreSQL container for contra accountsdb
        let node_postgres_container = Postgres::default()
            .with_db_name("contra_node")
            .with_user("postgres")
            .with_password("password")
            .start()
            .await
            .expect("Failed to start node PostgreSQL container");

        let node_host = node_postgres_container
            .get_host()
            .await
            .expect("Failed to get node host");
        let node_port = node_postgres_container
            .get_host_port_ipv4(5432)
            .await
            .expect("Failed to get node port");
        let node_db_url = format!(
            "postgres://postgres:password@{}:{}/contra_node",
            node_host, node_port
        );

        let test_context = setup(node_db_url.clone()).await.unwrap();
        test_suite(&test_context.contra_ctx, &test_context.l1_ctx).await;
        shutdown(test_context).await;

        // Dedup persistence test runs with its own node instance against the same DB
        run_dedup_persistence_test(node_db_url).await;
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_signature_statuses_only_with_postgres() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let node_postgres_container = Postgres::default()
            .with_db_name("contra_node")
            .with_user("postgres")
            .with_password("password")
            .start()
            .await
            .expect("Failed to start node PostgreSQL container");

        let node_host = node_postgres_container
            .get_host()
            .await
            .expect("Failed to get node host");
        let node_port = node_postgres_container
            .get_host_port_ipv4(5432)
            .await
            .expect("Failed to get node port");
        let node_db_url = format!(
            "postgres://postgres:password@{}:{}/contra_node",
            node_host, node_port
        );

        let test_context = setup(node_db_url).await.unwrap();
        run_get_signature_statuses_test(&test_context.contra_ctx).await;
        shutdown(test_context).await;
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_with_redis() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        // Start Redis container for contra accountsdb
        let redis_container = Redis::default()
            .with_tag("7")
            .start()
            .await
            .expect("Failed to start Redis container");

        let redis_host = redis_container
            .get_host()
            .await
            .expect("Failed to get host");
        let redis_port = redis_container
            .get_host_port_ipv4(6379)
            .await
            .expect("Failed to get port");
        let redis_url = format!("redis://{}:{}", redis_host, redis_port);

        println!("Redis container started at: {}", redis_url);

        let test_context = setup(redis_url).await.unwrap();
        test_suite(&test_context.contra_ctx, &test_context.l1_ctx).await;

        shutdown(test_context).await;
    })
    .await
    .unwrap();
}

/// Startup is organized into three parallel stages to minimize wall-clock time:
///   1. Validator + both Postgres containers  (all independent)
///   2. Both indexers              (each has its own DB and datasource)
///   3. Both operators             (independent of each other)
async fn setup(accountsdb_connection_url: String) -> Result<TestContext> {
    // Acquire global setup lock to serialize test initialization.
    // With nextest each test runs in its own process so this never blocks across
    // tests; it only guards against concurrent calls within the same process.
    let _lock = SETUP_LOCK.lock().await;

    // Generate keys before launching anything async
    let operator_key = Keypair::new();
    let mint = Pubkey::new_unique();
    let escrow_instance = Keypair::new();
    println!("\n=== SPL Token Integration Test (Postgres + Indexer) ===");
    println!("Operator: {}", operator_key.pubkey());
    println!("Mint: {}", mint);

    // Start the validator and both indexer Postgres containers in parallel —
    // they are fully independent of each other.
    println!("Starting validator and indexer databases in parallel...");
    let (
        (test_validator, faucet_keypair, geyser_port),
        contra_indexer_postgres_container,
        l1_indexer_postgres_container,
    ) = tokio::join!(
        start_test_validator(),
        Postgres::default()
            .with_db_name("contra_indexer")
            .with_user("postgres")
            .with_password("password")
            .start(),
        Postgres::default()
            .with_db_name("l1_indexer")
            .with_user("postgres")
            .with_password("password")
            .start(),
    );
    let contra_indexer_postgres_container =
        contra_indexer_postgres_container.expect("Failed to start Contra PostgreSQL container");
    let l1_indexer_postgres_container =
        l1_indexer_postgres_container.expect("Failed to start L1 PostgreSQL container");

    println!(
        "Solana test validator started on {}",
        test_validator.rpc_url()
    );
    println!("Geyser plugin running on port {}", geyser_port);

    // Resolve DB URLs now that the containers are up
    let contra_indexer_host = contra_indexer_postgres_container
        .get_host()
        .await
        .expect("Failed to get host");
    let contra_indexer_port = contra_indexer_postgres_container
        .get_host_port_ipv4(5432)
        .await
        .expect("Failed to get port");
    let contra_indexer_db_url = format!(
        "postgres://postgres:password@{}:{}/contra_indexer",
        contra_indexer_host, contra_indexer_port
    );

    let l1_indexer_host = l1_indexer_postgres_container
        .get_host()
        .await
        .expect("Failed to get host");
    let l1_indexer_port = l1_indexer_postgres_container
        .get_host_port_ipv4(5432)
        .await
        .expect("Failed to get port");
    let l1_indexer_db_url = format!(
        "postgres://postgres:password@{}:{}/l1_indexer",
        l1_indexer_host, l1_indexer_port
    );

    // Start the Contra node (requires the validator URL)
    let node_config = NodeConfig {
        mode: NodeMode::Aio,
        port: get_free_port(),
        sigverify_queue_size: 100,
        sigverify_workers: 2,
        max_connections: 50,
        max_tx_per_batch: 10,
        accountsdb_connection_url: accountsdb_connection_url.clone(),
        admin_keys: vec![operator_key.pubkey()],
        transaction_expiration_ms: 15000,
        blocktime_ms: 100,
        perf_sample_period_secs: 10, // Collect performance samples every 10 seconds for testing
    };
    let (contra_handles, contra_rpc_url) = start_contra(node_config).await.unwrap();

    // Derive instance PDA
    let (instance_pda, _instance_bump) = Pubkey::find_program_address(
        &[b"instance", escrow_instance.pubkey().as_ref()],
        &CONTRA_ESCROW_PROGRAM_ID,
    );

    // Start both indexers in parallel — each has its own DB and datasource
    println!("\n=== Starting Contra Indexer and L1 Indexer in parallel ===");
    let geyser_endpoint = format!("http://127.0.0.1:{}", geyser_port);
    let (contra_indexer_result, l1_indexer_result) = tokio::join!(
        start_contra_indexer(None, contra_rpc_url.clone(), contra_indexer_db_url.clone()),
        start_l1_indexer(
            geyser_endpoint,
            test_validator.rpc_url(),
            l1_indexer_db_url.clone(),
            Some(instance_pda),
        ),
    );
    let (contra_indexer_handle, contra_indexer_storage) =
        contra_indexer_result.expect("Failed to start Contra indexer");
    let (l1_indexer_handle, l1_indexer_storage) =
        l1_indexer_result.expect("Failed to start L1 indexer");
    println!("Contra Indexer and L1 Indexer started successfully");

    // Start both operators in parallel — they are independent of each other
    println!("\n=== Starting Operators in parallel ===");
    let operator_key_l1_to_contra = Keypair::try_from(&operator_key.to_bytes()[..]).unwrap();
    let operator_key_contra_to_l1 = Keypair::try_from(&operator_key.to_bytes()[..]).unwrap();
    let (l1_to_contra_result, contra_to_l1_result) = tokio::join!(
        start_l1_to_contra_operator(
            contra_rpc_url.clone(),
            l1_indexer_db_url.clone(),
            operator_key_l1_to_contra,
            instance_pda,
        ),
        start_contra_to_l1_operator(
            test_validator.rpc_url(),
            contra_indexer_db_url.clone(),
            operator_key_contra_to_l1,
            instance_pda,
        ),
    );
    let l1_to_contra_operator_handle =
        l1_to_contra_result.expect("Failed to start L1 -> Contra operator");
    let contra_to_l1_operator_handle =
        contra_to_l1_result.expect("Failed to start Contra -> L1 operator");
    println!("L1 -> Contra and Contra -> L1 Operators started successfully");

    let operator_key_clone = Keypair::try_from(&operator_key.to_bytes()[..]).unwrap();
    let l1_ctx = L1Context::new(
        test_validator.rpc_url(),
        operator_key_clone,
        faucet_keypair,
        escrow_instance,
        l1_indexer_storage,
    );
    let operator_key_clone = Keypair::try_from(&operator_key.to_bytes()[..]).unwrap();
    let contra_ctx = ContraContext::new(
        contra_rpc_url.clone(),
        contra_rpc_url.clone(),
        operator_key_clone,
        mint,
        contra_indexer_storage,
    );

    Ok(TestContext {
        _keep_alive: KeepAlive {
            _test_validator: test_validator,
            _contra_indexer_db: contra_indexer_postgres_container,
            _l1_indexer_db: l1_indexer_postgres_container,
        },
        l1_to_contra_operator_handle,
        contra_to_l1_operator_handle,
        contra_indexer_handle,
        l1_indexer_handle,
        contra_handles,
        contra_ctx,
        l1_ctx,
    })
}

async fn test_suite(contra_ctx: &ContraContext, l1_ctx: &L1Context) {
    run_precompile_accounts_test(contra_ctx).await;
    run_spl_token_test(contra_ctx, l1_ctx, spl_token::ID).await;
    run_spl_token_test(contra_ctx, l1_ctx, spl_token_2022::ID).await;
    run_tx_replay_test(contra_ctx).await;
    run_transaction_count_test(contra_ctx).await;
    run_get_transaction_test(contra_ctx).await;
    run_first_available_block_test(contra_ctx).await;
    run_get_blocks_test(contra_ctx).await;
    run_get_signature_statuses_test(contra_ctx).await;
    run_get_block_time_test(contra_ctx).await;
    run_get_slot_leaders_test(contra_ctx).await;
    run_epoch_info_test(contra_ctx).await;
    run_epoch_schedule_test(contra_ctx).await;
    run_vote_accounts_test(contra_ctx).await;
    run_get_supply_test(contra_ctx).await;
    run_blockhash_validation_test(contra_ctx).await;
    run_non_admin_sending_admin_instruction_test(contra_ctx).await;
    run_empty_transaction_test(contra_ctx).await;
    run_mixed_transaction_test(contra_ctx).await;
    // Must be last to collect all samples
    run_performance_samples_test(contra_ctx).await;
}

async fn shutdown(test_context: TestContext) {
    println!("\n=== Shutting Down ===");
    drop(test_context._keep_alive);
    test_context.l1_to_contra_operator_handle.shutdown().await;
    test_context.contra_to_l1_operator_handle.shutdown().await;
    test_context.contra_indexer_handle.abort();
    test_context.l1_indexer_handle.abort();
    test_context.contra_handles.shutdown().await;
}
