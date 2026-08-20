//! Startup anchor wiring for `private_channel_indexer::indexer::run`.
//!
//! Reconnect gap repair replays from the durable checkpoint, so `run` must persist one
//! before the Yellowstone stream can deliver anything. These tests pin the value it picks:
//! the chain tip when no startup backfill resolved a boundary, and the boundary's floor
//! when one did. Picking the top of the resolved range instead would durably claim the
//! slots backfill has not fetched yet, which is the loss the anchor exists to prevent.

use mockito::{Matcher, Server as MockitoServer};
use private_channel_indexer::{
    config::{BackfillConfig, ReconciliationConfig, RpcPollingConfig, YellowstoneConfig},
    indexer::run,
    DatasourceType, IndexerConfig, PostgresConfig, PrivateChannelIndexerConfig, ProgramType,
    StorageType,
};
use serde_json::json;
use solana_sdk::commitment_config::CommitmentLevel;
use solana_transaction_status::UiTransactionEncoding;
use sqlx::PgPool;
use std::time::Duration;
use test_utils::mock_yellowstone::MockYellowstoneServer;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;

/// Chain tip both tests' mock RPC reports.
const TIP: u64 = 900;
/// First slot to process when backfill is on, so the floor 897 stays distinct from the tip.
const START_SLOT: u64 = 898;

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info,private_channel_indexer=debug")
        .with_test_writer()
        .try_init();
}

/// Real Postgres via testcontainers; the container must stay alive for the test.
async fn start_postgres(
    db: &str,
) -> (
    testcontainers::ContainerAsync<Postgres>,
    PgPool,
    PostgresConfig,
) {
    let pg = Postgres::default()
        .with_db_name(db)
        .with_user("postgres")
        .with_password("password")
        .start()
        .await
        .expect("postgres container");
    let host = pg.get_host().await.expect("pg host");
    let port = pg.get_host_port_ipv4(5432).await.expect("pg port");
    let url = format!("postgres://postgres:password@{host}:{port}/{db}");

    let pool = PgPool::connect(&url).await.expect("pg pool");
    let config = PostgresConfig {
        database_url: url,
        max_connections: 10,
    };
    (pg, pool, config)
}

/// Read the anchor row. `run` creates the schema, so an early query is a "not yet".
async fn anchor_of(pool: &PgPool, program: &str) -> Option<u64> {
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT last_committed_slot FROM indexer_state WHERE program_type = $1")
            .bind(program)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    row.map(|(slot,)| slot as u64)
}

/// Poll until the anchor row appears, so the assertion does not race startup.
async fn wait_for_anchor(pool: &PgPool, program: &str, secs: u64) -> u64 {
    tokio::time::timeout(Duration::from_secs(secs), async {
        loop {
            if let Some(slot) = anchor_of(pool, program).await {
                return slot;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no startup anchor was ever persisted for {program}"))
}

/// Yellowstone config pointed at a mock that never sends a block, plus the given backfill
/// settings. Withdraw skips escrow reconciliation, which needs an on-chain instance.
/// The rpc_polling block is unused by the Yellowstone datasource but is required whenever
/// backfill is enabled, so it is always supplied.
fn indexer_config(geyser_endpoint: String, backfill: BackfillConfig) -> IndexerConfig {
    IndexerConfig {
        datasource_type: DatasourceType::Yellowstone,
        rpc_polling: Some(RpcPollingConfig {
            from_slot: None,
            poll_interval_ms: 1_000,
            error_retry_interval_ms: 1_000,
            batch_size: 10,
            encoding: UiTransactionEncoding::Json,
            commitment: CommitmentLevel::Finalized,
        }),
        yellowstone: Some(YellowstoneConfig {
            endpoint: geyser_endpoint,
            x_token: None,
            commitment: "confirmed".to_string(),
        }),
        backfill,
        reconciliation: ReconciliationConfig::default(),
    }
}

fn common_config(postgres: PostgresConfig, rpc_url: String) -> PrivateChannelIndexerConfig {
    PrivateChannelIndexerConfig {
        program_type: ProgramType::Withdraw,
        storage_type: StorageType::Postgres,
        rpc_url,
        source_rpc_url: None,
        fallback_rpc_url: None,
        postgres,
        escrow_instance_id: None,
    }
}

async fn mock_get_slot(rpc: &mut MockitoServer, slot: u64) -> mockito::Mock {
    rpc.mock("POST", "/")
        .match_body(Matcher::PartialJson(json!({"method": "getSlot"})))
        .with_status(200)
        .with_body(json!({"jsonrpc": "2.0", "result": slot, "id": 1}).to_string())
        .create_async()
        .await
}

/// The shipped Yellowstone deployment runs with backfill disabled. Nothing resolves a
/// boundary there, so the tip the stream is about to start from is the anchor, and it has
/// to be durable before the first block rather than after the checkpoint writer flushes.
/// The mock never sends a block, so a row appearing at all can only have come from startup.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_persists_tip_anchor_before_streaming() {
    init_tracing();
    let (_pg, pool, postgres) = start_postgres("startup_anchor_tip").await;

    let mut rpc = MockitoServer::new_async().await;
    let _slot = mock_get_slot(&mut rpc, TIP).await;
    // No block mocks: any fetch would mean a backfill ran where none was configured.
    let no_blocks = rpc
        .mock("POST", "/")
        .match_body(Matcher::PartialJson(json!({"method": "getBlock"})))
        .expect(0)
        .create_async()
        .await;

    let ys = MockYellowstoneServer::start().await;

    let backfill = BackfillConfig {
        enabled: false,
        exit_after_backfill: false,
        rpc_url: rpc.url(),
        batch_size: 100,
        max_gap_slots: 1_000,
        start_slot: None,
    };
    let indexer = indexer_config(ys.url(), backfill);
    let common = common_config(postgres, rpc.url());

    // Surface a startup failure instead of letting it look like a missing anchor.
    let handle = tokio::spawn(async move {
        if let Err(e) = run(common, indexer, None).await {
            eprintln!("indexer run exited: {e}");
        }
    });

    // The mock never sends a block, so this anchor can only have come from startup.
    let anchor = wait_for_anchor(&pool, "withdraw", 60).await;
    assert_eq!(
        anchor, TIP,
        "a backfill-disabled deployment must anchor at the tip it starts streaming from"
    );
    no_blocks.assert_async().await;

    handle.abort();
    ys.shutdown().await;
}

/// With startup backfill resolving a range, the anchor is that range's floor. The tip and
/// the live start slot both sit above slots backfill has yet to fetch, so recording either
/// would mark them handled and put them permanently out of reach. All three are plain u64,
/// so nothing but this test stops the wrong one being passed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_anchors_at_resolved_from_slot_not_the_tip() {
    init_tracing();
    let (_pg, pool, postgres) = start_postgres("startup_anchor_floor").await;

    let floor = START_SLOT - 1;

    let mut rpc = MockitoServer::new_async().await;
    let _slot = mock_get_slot(&mut rpc, TIP).await;
    let _enumeration = rpc
        .mock("POST", "/")
        .match_body(Matcher::PartialJson(
            json!({"method": "getBlocks", "params": [START_SLOT, TIP]}),
        ))
        .with_status(200)
        .with_body(json!({"jsonrpc": "2.0", "result": [898, 899, 900], "id": 1}).to_string())
        .expect_at_least(0)
        .create_async()
        .await;
    for slot in START_SLOT..=TIP {
        let _block = rpc
            .mock("POST", "/")
            .match_body(Matcher::PartialJson(
                json!({"method": "getBlock", "params": [slot]}),
            ))
            .with_status(200)
            .with_body(
                json!({
                    "jsonrpc": "2.0",
                    "result": {
                        "blockhash": "TestBlockHash11111111111111111111111111111",
                        "parentSlot": slot - 1,
                        "transactions": []
                    },
                    "id": 1
                })
                .to_string(),
            )
            .expect_at_least(0)
            .create_async()
            .await;
    }

    let ys = MockYellowstoneServer::start().await;

    let backfill = BackfillConfig {
        enabled: true,
        exit_after_backfill: false,
        rpc_url: rpc.url(),
        batch_size: 100,
        max_gap_slots: 1_000,
        start_slot: Some(START_SLOT),
    };
    let indexer = indexer_config(ys.url(), backfill);
    let common = common_config(postgres, rpc.url());

    // Surface a startup failure instead of letting it look like a missing anchor.
    let handle = tokio::spawn(async move {
        if let Err(e) = run(common, indexer, None).await {
            eprintln!("indexer run exited: {e}");
        }
    });

    let anchor = wait_for_anchor(&pool, "withdraw", 60).await;
    assert_eq!(
        anchor,
        floor,
        "the anchor must be the resolved range floor, not the tip ({TIP}) \
         or the live start slot ({})",
        TIP + 1
    );

    handle.abort();
    ys.shutdown().await;
}
