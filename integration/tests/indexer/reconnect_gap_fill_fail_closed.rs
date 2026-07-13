//! Fail-closed reconnect gap-fill for `YellowstoneSource`: after a stream drop the
//! source must not resubscribe until the gap `(checkpoint, tip]` is repaired, so no
//! live `SlotComplete` can advance the durable checkpoint over the missing range.

use mockito::{Matcher, Server as MockitoServer};
use private_channel_indexer::config::ProgramType;
use private_channel_indexer::indexer::checkpoint::CheckpointWriter;
use private_channel_indexer::indexer::datasource::common::datasource::DataSource;
use private_channel_indexer::indexer::datasource::common::types::ProcessorMessage;
use private_channel_indexer::indexer::datasource::rpc_polling::rpc::RpcPoller;
use private_channel_indexer::indexer::datasource::yellowstone::YellowstoneSource;
use private_channel_indexer::indexer::transaction_processor::TransactionProcessor;
use private_channel_indexer::storage::{PostgresDb, Storage};
use private_channel_indexer::PostgresConfig;
use serde_json::json;
use solana_sdk::commitment_config::CommitmentLevel;
use solana_transaction_status::UiTransactionEncoding;
use std::sync::Arc;
use std::time::Duration;
use test_utils::mock_yellowstone::{MockYellowstoneServer, Update, UpdateMatcher};
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[path = "yellowstone_helpers.rs"]
mod yellowstone_helpers;
use yellowstone_helpers::empty_block;

const CHECKPOINT: u64 = 100;
const TIP: u64 = 103;

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info,private_channel_indexer=debug")
        .with_test_writer()
        .try_init();
}

/// Real Postgres via testcontainers; the container must stay alive for the test.
async fn start_postgres() -> (testcontainers::ContainerAsync<Postgres>, Arc<Storage>) {
    let pg = Postgres::default()
        .with_db_name("reconnect_gap")
        .with_user("postgres")
        .with_password("password")
        .start()
        .await
        .expect("postgres container");
    let host = pg.get_host().await.expect("pg host");
    let port = pg.get_host_port_ipv4(5432).await.expect("pg port");
    let db_url = format!("postgres://postgres:password@{host}:{port}/reconnect_gap");

    let storage = Storage::Postgres(
        PostgresDb::new(&PostgresConfig {
            database_url: db_url,
            max_connections: 10,
        })
        .await
        .expect("postgres storage"),
    );
    storage.init_schema().await.expect("init schema");
    (pg, Arc::new(storage))
}

fn empty_block_json() -> serde_json::Value {
    json!({
        "blockhash": "TestBlockHash11111111111111111111111111111",
        "parentSlot": 0,
        "transactions": []
    })
}

async fn mock_get_slot(server: &mut MockitoServer, slot: u64) -> mockito::Mock {
    server
        .mock("POST", "/")
        .match_body(Matcher::PartialJson(json!({"method": "getSlot"})))
        .with_status(200)
        .with_body(json!({"jsonrpc": "2.0", "result": slot, "id": 1}).to_string())
        .expect_at_least(1)
        .create_async()
        .await
}

async fn mock_block_ok(server: &mut MockitoServer, slot: u64) -> mockito::Mock {
    server
        .mock("POST", "/")
        .match_body(Matcher::PartialJson(
            json!({"method": "getBlock", "params": [slot]}),
        ))
        .with_status(200)
        .with_body(json!({"jsonrpc": "2.0", "result": empty_block_json(), "id": 1}).to_string())
        .create_async()
        .await
}

/// Slot answers -32009 below the retention floor, so the poller reports it
/// Unavailable and the fill aborts before any SlotComplete.
async fn mock_block_pruned(
    server: &mut MockitoServer,
    slot: u64,
    min_hits: usize,
) -> mockito::Mock {
    server
        .mock("POST", "/")
        .match_body(Matcher::PartialJson(
            json!({"method": "getBlock", "params": [slot]}),
        ))
        .with_status(200)
        .with_body(
            json!({
                "jsonrpc": "2.0",
                "error": { "code": -32009, "message": "Slot was skipped" },
                "id": 1
            })
            .to_string(),
        )
        .expect_at_least(min_hits)
        .create_async()
        .await
}

async fn mock_retention_floor(server: &mut MockitoServer, floor: u64) -> mockito::Mock {
    server
        .mock("POST", "/")
        .match_body(Matcher::PartialJson(
            json!({"method": "getFirstAvailableBlock"}),
        ))
        .with_status(200)
        .with_body(json!({"jsonrpc": "2.0", "result": floor, "id": 1}).to_string())
        .create_async()
        .await
}

async fn wait_for_subscribes(ys: &MockYellowstoneServer, n: usize, secs: u64) {
    tokio::time::timeout(Duration::from_secs(secs), async {
        while ys.call_count("subscribe") < n {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "expected {} subscribe handshakes, got {}",
            n,
            ys.call_count("subscribe")
        )
    });
}

async fn wait_until_matched(mock: &mockito::Mock, secs: u64, what: &str) {
    tokio::time::timeout(Duration::from_secs(secs), async {
        while !mock.matched_async().await {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for: {what}"));
}

fn poller(server: &MockitoServer) -> Arc<RpcPoller> {
    Arc::new(RpcPoller::new(
        server.url(),
        UiTransactionEncoding::Json,
        CommitmentLevel::Confirmed,
    ))
}

/// While the fill fails the source stalls fail-closed (no resubscribe, no
/// SlotComplete, checkpoint frozen); once the RPC heals, the gap slots land
/// first and only then does live delivery resume on subscribe #2.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stalls_while_gap_fill_fails_then_recovers_in_order() {
    init_tracing();
    let (_pg, storage) = start_postgres().await;
    storage
        .update_committed_checkpoint("escrow", CHECKPOINT)
        .await
        .expect("seed checkpoint");

    let mut rpc = MockitoServer::new_async().await;
    let _slot = mock_get_slot(&mut rpc, TIP).await;
    for s in (CHECKPOINT + 1)..=TIP {
        let _m = mock_block_ok(&mut rpc, s).await;
    }
    // The boundary slot is the blocker: expect the initial attempt plus at
    // least one retry, proving the loop retries instead of falling through.
    let pruned = mock_block_pruned(&mut rpc, CHECKPOINT, 2).await;
    let _floor = mock_retention_floor(&mut rpc, 500).await;

    let ys = MockYellowstoneServer::start().await;
    let (tx, mut rx) = mpsc::channel::<ProcessorMessage>(256);
    let cancel = CancellationToken::new();

    let mut source = YellowstoneSource::new(
        ys.url(),
        None,
        "confirmed".to_string(),
        ProgramType::Escrow,
        None,
    )
    .with_gap_detection(poller(&rpc), 1_000, 16)
    .with_storage(storage.clone());

    let handle = source
        .start(tx, cancel.clone())
        .await
        .expect("yellowstone source start");

    wait_for_subscribes(&ys, 1, 10).await;
    ys.drop_stream();

    // Failing phase: two attempts on the pruned slot means one full 5s backoff
    // elapsed with the gap still open.
    wait_until_matched(&pruned, 20, "two gap-fill attempts on the pruned slot").await;

    assert_eq!(
        ys.call_count("subscribe"),
        1,
        "must not resubscribe while the gap is open"
    );
    while let Ok(msg) = rx.try_recv() {
        assert!(
            !matches!(msg, ProcessorMessage::SlotComplete { .. }),
            "no SlotComplete may be emitted while the gap-fill is failing"
        );
    }
    assert_eq!(
        storage
            .get_committed_checkpoint("escrow")
            .await
            .expect("read checkpoint"),
        Some(CHECKPOINT),
        "durable checkpoint must stay frozen during the stall"
    );

    // Heal: mockito matches the newest-registered mock first, so slot 100 now
    // serves an empty block. Queue a live slot for the post-repair stream.
    let _healed = mock_block_ok(&mut rpc, CHECKPOINT).await;
    ys.enqueue(UpdateMatcher, Update::ok(empty_block(TIP + 1)));

    let mut slots = vec![];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while slots.len() < 5 {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            panic!("timed out waiting for recovery; slots so far: {slots:?}");
        }
        if let Ok(Some(ProcessorMessage::SlotComplete { slot, .. })) =
            tokio::time::timeout(remaining, rx.recv()).await
        {
            slots.push(slot);
        }
    }
    assert_eq!(
        slots,
        vec![100, 101, 102, 103, 104],
        "gap slots must land before the first live slot of subscribe #2"
    );
    assert!(
        ys.call_count("subscribe") >= 2,
        "the source must resubscribe only after the gap is repaired"
    );

    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(3), handle).await;
    ys.shutdown().await;
}

/// Cancellation during a stalled gap-fill joins the source task promptly
/// and stops all RPC traffic.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_while_gap_fill_stalled_joins_promptly() {
    init_tracing();
    let (_pg, storage) = start_postgres().await;
    storage
        .update_committed_checkpoint("escrow", CHECKPOINT)
        .await
        .expect("seed checkpoint");

    let mut rpc = MockitoServer::new_async().await;
    let _slot = mock_get_slot(&mut rpc, TIP).await;
    for s in (CHECKPOINT + 1)..=TIP {
        let _m = mock_block_ok(&mut rpc, s).await;
    }
    let pruned = mock_block_pruned(&mut rpc, CHECKPOINT, 1).await;
    let _floor = mock_retention_floor(&mut rpc, 500).await;

    let ys = MockYellowstoneServer::start().await;
    let (tx, _rx) = mpsc::channel::<ProcessorMessage>(256);
    let cancel = CancellationToken::new();

    let mut source = YellowstoneSource::new(
        ys.url(),
        None,
        "confirmed".to_string(),
        ProgramType::Escrow,
        None,
    )
    .with_gap_detection(poller(&rpc), 1_000, 16)
    .with_storage(storage.clone());

    let handle = source
        .start(tx, cancel.clone())
        .await
        .expect("yellowstone source start");

    wait_for_subscribes(&ys, 1, 10).await;
    ys.drop_stream();
    wait_until_matched(&pruned, 20, "a failed gap-fill attempt").await;

    // Cancel mid-stall; the select! on the backoff must wake immediately, far
    // inside the 5s production backoff.
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(3), handle)
        .await
        .expect("source must join promptly when cancelled during a stall")
        .expect("source task must not panic");

    // Newest-registered mock matches first: any RPC call after the join would
    // land here and fail the zero-hit expectation.
    let quiet = rpc.mock("POST", "/").expect(0).create_async().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    quiet.assert_async().await;

    ys.shutdown().await;
}

/// The exact bug (#21) end-to-end: a live tip beyond the gap must never advance
/// the durable checkpoint while the gap is unfilled. This wires the real
/// processor and the ungated plain-max checkpoint writer, the component that
/// corrupted the checkpoint in the bug, so the DB value is the observable. The
/// boundary slot stays pruned for the whole test, so gap-fill can never resolve;
/// a live block past the tip is queued but only a resubscribe would deliver it,
/// and a fail-open regression would resubscribe and leap the checkpoint to it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stalled_gap_fill_never_advances_checkpoint_over_live_tip() {
    init_tracing();
    let (_pg, storage) = start_postgres().await;
    storage
        .update_committed_checkpoint("escrow", CHECKPOINT)
        .await
        .expect("seed checkpoint");

    let mut rpc = MockitoServer::new_async().await;
    let _slot = mock_get_slot(&mut rpc, TIP).await;
    for s in (CHECKPOINT + 1)..=TIP {
        let _m = mock_block_ok(&mut rpc, s).await;
    }
    // Never healed: the gap stays open for the whole test.
    let pruned = mock_block_pruned(&mut rpc, CHECKPOINT, 1).await;
    let _floor = mock_retention_floor(&mut rpc, 500).await;

    // Real pipeline: the processor finalizes slots and the ungated writer
    // plain-maxes each SlotComplete into the durable checkpoint.
    let (checkpoint_tx, checkpoint_rx) = mpsc::channel(64);
    let writer_handle = CheckpointWriter::new(storage.clone())
        .with_batch_interval(1)
        .start(checkpoint_rx);
    let (tx, instruction_rx) = mpsc::channel::<ProcessorMessage>(256);
    let processor = TransactionProcessor::new(storage.clone(), checkpoint_tx);
    let processor_handle = tokio::spawn(processor.start(instruction_rx));

    let ys = MockYellowstoneServer::start().await;
    let cancel = CancellationToken::new();

    let mut source = YellowstoneSource::new(
        ys.url(),
        None,
        "confirmed".to_string(),
        ProgramType::Escrow,
        None,
    )
    .with_gap_detection(poller(&rpc), 1_000, 16)
    .with_storage(storage.clone());

    let handle = source
        .start(tx.clone(), cancel.clone())
        .await
        .expect("yellowstone source start");

    wait_for_subscribes(&ys, 1, 10).await;
    ys.drop_stream();

    // Queued after the drop, so only a resubscribe (subscribe #2) could deliver
    // it. Under the fix that never happens; under the old fail-open it would land
    // and push the checkpoint to TIP + 1.
    ys.enqueue(UpdateMatcher, Update::ok(empty_block(TIP + 1)));
    wait_until_matched(&pruned, 20, "a gap-fill attempt on the pruned slot").await;

    // Hold well past the 5s backoff so a regressed fail-open would have
    // resubscribed and advanced the checkpoint by now; it must stay at the seed.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        assert_eq!(
            storage
                .get_committed_checkpoint("escrow")
                .await
                .expect("read checkpoint"),
            Some(CHECKPOINT),
            "checkpoint must never advance past the unfilled gap"
        );
        assert_eq!(
            ys.call_count("subscribe"),
            1,
            "the source must not resubscribe while the gap is open"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(3), handle).await;
    drop(tx);
    let _ = tokio::time::timeout(Duration::from_secs(3), processor_handle).await;
    let _ = tokio::time::timeout(Duration::from_secs(3), writer_handle).await;
    ys.shutdown().await;
}
