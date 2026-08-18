//! Reconnect-gap recovery for `YellowstoneSource`.
//!
//! After a Yellowstone disconnect, the source reads the durable checkpoint
//! from storage and passes `checkpoint - 1`
//! to `fill_slot_range` so the boundary slot is replayed via RPC. Tx/mint
//! inserts are idempotent, so replaying is safe.

use mockito::{Matcher, Server as MockitoServer};
use private_channel_indexer::config::ProgramType;
use private_channel_indexer::indexer::datasource::common::datasource::DataSource;
use private_channel_indexer::indexer::datasource::common::types::ProcessorMessage;
use private_channel_indexer::indexer::datasource::rpc_polling::rpc::RpcPoller;
use private_channel_indexer::indexer::datasource::yellowstone::YellowstoneSource;
use private_channel_indexer::storage::common::storage::mock::MockStorage;
use private_channel_indexer::storage::Storage;
use serde_json::json;
use solana_sdk::commitment_config::CommitmentLevel;
use solana_transaction_status::UiTransactionEncoding;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use test_utils::mock_yellowstone::{MockYellowstoneServer, Update, UpdateMatcher};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[path = "yellowstone_helpers.rs"]
mod yellowstone_helpers;
use yellowstone_helpers::empty_block;

fn empty_block_json() -> serde_json::Value {
    json!({
        "blockhash": "TestBlockHash11111111111111111111111111111",
        "parentSlot": 0,
        "transactions": []
    })
}

/// Happy-path: checkpoint=101 → stream 100,101 → drop → backfill 101..=106
/// inclusive (anchor = checkpoint-1 = 100) → resume streaming 107,108.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gap_fill_runs_after_drop_stream() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info,private_channel_indexer=debug")
        .with_test_writer()
        .try_init();

    // In-process mockito RPC backend for the RpcPoller backfill path.
    let mut rpc_mock = MockitoServer::new_async().await;

    // The fill targets the observed resume slot (106 below), not a tip probe, so
    // no getSlot mock is needed. Anchor = checkpoint-1 = 100 => backfill 101..=106.
    // Empty blocks => only SlotComplete markers. Slot 101 was also streamed;
    // replay is harmless thanks to idempotent inserts in prod.
    // v2 enumerates the batch before fetching it, so this mock is required even
    // though nothing here is absent. Unmocked, mockito answers 501 with an empty
    // body, which becomes a decode error the gap-fill retry loop swallows.
    let _enumeration = rpc_mock
        .mock("POST", "/")
        .match_body(Matcher::PartialJson(
            json!({"method": "getBlocks", "params": [101, 106]}),
        ))
        .with_status(200)
        .with_body(
            json!({"jsonrpc": "2.0", "result": [101, 102, 103, 104, 105, 106], "id": 1})
                .to_string(),
        )
        .expect_at_least(1)
        .create_async()
        .await;

    let mut block_mocks = Vec::new();
    for slot in 101u64..=106u64 {
        let m = rpc_mock
            .mock("POST", "/")
            .match_body(Matcher::PartialJson(
                json!({"method": "getBlock", "params": [slot]}),
            ))
            .with_status(200)
            .with_body(
                json!({
                    "jsonrpc": "2.0",
                    "result": empty_block_json(),
                    "id": 1,
                })
                .to_string(),
            )
            .expect_at_least(1)
            .create_async()
            .await;
        block_mocks.push(m);
    }

    let server = MockYellowstoneServer::start().await;

    let rpc_poller = Arc::new(RpcPoller::new(
        rpc_mock.url(),
        UiTransactionEncoding::Json,
        CommitmentLevel::Confirmed,
    ));

    // Pre-seed durable checkpoint = 101. In prod the processor advances it.
    let mock_storage = MockStorage::new();
    mock_storage.set_checkpoint("escrow", 101);
    let storage: Arc<Storage> = Arc::new(Storage::Mock(mock_storage));

    let (tx, mut rx) = mpsc::channel::<ProcessorMessage>(256);
    let cancel = CancellationToken::new();

    let mut source = YellowstoneSource::new(
        server.url(),
        None,
        "confirmed".to_string(),
        ProgramType::Escrow,
        None,
    )
    .with_gap_detection(rpc_poller, 1_000, 16)
    .with_storage(storage);

    let handle = source
        .start(tx, cancel.clone())
        .await
        .expect("yellowstone source start");

    // Phase 1: deliver slots 100, 101 pre-disconnect.
    server.enqueue(UpdateMatcher, Update::ok(empty_block(100)));
    server.enqueue(UpdateMatcher, Update::ok(empty_block(101)));

    // Collect both initial slots.
    let mut seen: HashSet<u64> = HashSet::new();
    let deadline_phase1 = tokio::time::Instant::now() + Duration::from_secs(5);
    while !(seen.contains(&100) && seen.contains(&101)) {
        let remaining = deadline_phase1.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            panic!("phase 1 timed out; seen: {:?}", seen);
        }
        if let Ok(Some(ProcessorMessage::SlotComplete { slot, .. })) =
            tokio::time::timeout(remaining, rx.recv()).await
        {
            seen.insert(slot);
        }
    }

    // Phase 2: drop the stream. On resubscribe the first live block (106) becomes the
    // gate target, and the concurrent backfill fills 101..=106.
    server.drop_stream();

    // Phase 3: queue 106,107,108. The 106 resume slot sets the fill target; 107,108
    // prove streaming continues past the backfilled window.
    server.enqueue(UpdateMatcher, Update::ok(empty_block(106)));
    server.enqueue(UpdateMatcher, Update::ok(empty_block(107)));
    server.enqueue(UpdateMatcher, Update::ok(empty_block(108)));

    // Expect 101..=106 from inclusive backfill + 107,108 from resumed stream.
    let deadline_phase2 = tokio::time::Instant::now() + Duration::from_secs(20);
    let wanted: HashSet<u64> = (101u64..=108u64).collect();
    while !wanted.is_subset(&seen) {
        let remaining = deadline_phase2.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            panic!(
                "phase 2 timed out waiting for backfill + resumed stream; \
                 seen so far: {:?}, missing: {:?}",
                seen,
                wanted.difference(&seen).collect::<Vec<_>>()
            );
        }
        if let Ok(Some(ProcessorMessage::SlotComplete { slot, .. })) =
            tokio::time::timeout(remaining, rx.recv()).await
        {
            seen.insert(slot);
        }
    }

    assert!(
        wanted.is_subset(&seen),
        "expected all gap + post-reconnect slots in processor channel; \
         seen: {:?}",
        seen
    );
    assert!(
        server.call_count("subscribe") >= 2,
        "drop_stream + resume should produce ≥2 subscribe handshakes; got {}",
        server.call_count("subscribe")
    );

    // Teardown.
    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(3), handle).await;
    server.shutdown().await;
}

/// The bug, end to end. With no durable anchor there is no lower bound to replay from, so
/// the slot the replacement stream resumes at must be withheld. Forwarding it is what used
/// to carry the checkpoint over the slots that arrived while nothing was listening, and
/// once the checkpoint passed them nothing revisited them on any later restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnect_without_anchor_withholds_live_slots() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info,private_channel_indexer=debug")
        .with_test_writer()
        .try_init();

    let mut rpc_mock = MockitoServer::new_async().await;

    // Any repair attempt would land here; without an anchor none may be made.
    let no_blocks = rpc_mock
        .mock("POST", "/")
        .match_body(Matcher::PartialJson(json!({"method": "getBlock"})))
        .expect(0)
        .create_async()
        .await;
    let _slot_mock = rpc_mock
        .mock("POST", "/")
        .match_body(Matcher::PartialJson(json!({"method": "getSlot"})))
        .with_status(200)
        .with_body(json!({"jsonrpc": "2.0", "result": 200, "id": 1}).to_string())
        .expect_at_most(1)
        .create_async()
        .await;

    let server = MockYellowstoneServer::start().await;

    let rpc_poller = Arc::new(RpcPoller::new(
        rpc_mock.url(),
        UiTransactionEncoding::Json,
        CommitmentLevel::Confirmed,
    ));

    // No checkpoint seeded, so the source has never recorded a recovery anchor.
    let storage: Arc<Storage> = Arc::new(Storage::Mock(MockStorage::new()));

    let (tx, mut rx) = mpsc::channel::<ProcessorMessage>(64);
    let cancel = CancellationToken::new();

    let mut source = YellowstoneSource::new(
        server.url(),
        None,
        "confirmed".to_string(),
        ProgramType::Escrow,
        None,
    )
    .with_gap_detection(rpc_poller, 1_000, 16)
    .with_storage(storage);

    let handle = source
        .start(tx, cancel.clone())
        .await
        .expect("yellowstone source start");

    // The first connection is a cold start and forwards its block without arming.
    server.enqueue(UpdateMatcher, Update::ok(empty_block(100)));
    let first = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("slot 100 must be forwarded on the first connection");
    assert!(
        matches!(
            first,
            Some(ProcessorMessage::SlotComplete { slot: 100, .. })
        ),
        "expected SlotComplete for slot 100, got {first:?}"
    );

    // Drop, then resume at a later slot. Slot 101 is the value-bearing slot nobody saw.
    server.drop_stream();
    server.enqueue(UpdateMatcher, Update::ok(empty_block(102)));

    // Hold well past the 5s retry backoff: a regression forwards 102 almost immediately.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
    let mut leaked = vec![];
    while tokio::time::Instant::now() < deadline {
        if let Ok(Some(ProcessorMessage::SlotComplete { slot, .. })) =
            tokio::time::timeout(Duration::from_millis(200), rx.recv()).await
        {
            leaked.push(slot);
        }
    }

    assert!(
        leaked.is_empty(),
        "no slot may be forwarded without a durable anchor; leaked: {leaked:?}"
    );
    no_blocks.assert_async().await;
    assert!(
        server.call_count("subscribe") >= 2,
        "the source resubscribes; the withheld block, not a blocked resubscribe, is the guard"
    );

    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(3), handle).await;
    server.shutdown().await;
}
