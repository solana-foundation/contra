//! Reconnect-gap recovery for `YellowstoneSource`.
//!
//! Exercises the production private_channelct documented at `source.rs::start`:
//! after a Yellowstone disconnect, when
//! configured with `.with_gap_detection(rpc_poller, max_gap_slots,
//! batch_size)`, the source should
//!
//!   1. detect the gap between the last streamed slot and the current
//!      chain tip (via `RpcPoller::get_latest_slot`),
//!   2. call `fill_slot_range` to fetch missed blocks via RPC, and
//!   3. emit `ProcessorMessage::SlotComplete` markers for every slot in
//!      the gap before the new streaming subscription resumes.
//!
//! The mock Yellowstone server provides a `drop_stream()` helper that
//! simulates a mid-subscription disconnect; an in-process `mockito` RPC
//! server stands in for the backfill RPC endpoint.

use mockito::{Matcher, Server as MockitoServer};
use private_channel_indexer::config::ProgramType;
use private_channel_indexer::indexer::datasource::common::datasource::DataSource;
use private_channel_indexer::indexer::datasource::common::types::ProcessorMessage;
use private_channel_indexer::indexer::datasource::rpc_polling::rpc::RpcPoller;
use private_channel_indexer::indexer::datasource::yellowstone::YellowstoneSource;
use serde_json::json;
use solana_sdk::commitment_config::CommitmentLevel;
use solana_transaction_status::UiTransactionEncoding;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use test_utils::mock_yellowstone::{MockYellowstoneServer, Update, UpdateMatcher};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use yellowstone_grpc_proto::geyser::{
    subscribe_update::UpdateOneof, SubscribeUpdate, SubscribeUpdateBlockMeta,
};

fn block_meta(slot: u64) -> SubscribeUpdate {
    SubscribeUpdate {
        filters: vec!["all_blocks_meta".to_string()],
        update_oneof: Some(UpdateOneof::BlockMeta(SubscribeUpdateBlockMeta {
            slot,
            blockhash: format!("hash-{slot}"),
            ..Default::default()
        })),
        created_at: None,
    }
}

fn empty_block_json() -> serde_json::Value {
    json!({
        "blockhash": "TestBlockHash11111111111111111111111111111",
        "parentSlot": 0,
        "transactions": []
    })
}

/// End-to-end reconnect-gap: stream N, N+1 → drop stream → after reconnect
/// the source should backfill N+2..=N+6 via the RpcPoller and then resume
/// streaming. Asserts every slot in [N, N+6] shows up on the processor
/// channel.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gap_fill_runs_after_drop_stream() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info,private_channel_indexer=debug")
        .with_test_writer()
        .try_init();

    // In-process mockito RPC backend for the RpcPoller backfill path.
    let mut rpc_mock = MockitoServer::new_async().await;

    // getSlot → current chain tip = 106. The reconnect-gap logic will see
    // last_seen_slot = 101 and ask for slots 102..=106.
    let _slot_mock = rpc_mock
        .mock("POST", "/")
        .match_body(Matcher::PartialJson(json!({"method": "getSlot"})))
        .with_status(200)
        .with_body(json!({"jsonrpc": "2.0", "result": 106, "id": 1}).to_string())
        .expect_at_least(1)
        .create_async()
        .await;

    // getBlock for each slot in the gap — empty blocks so parse_block emits
    // no instructions, only SlotComplete markers.
    let mut block_mocks = Vec::new();
    for slot in 102u64..=106u64 {
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

    let (tx, mut rx) = mpsc::channel::<ProcessorMessage>(256);
    let cancel = CancellationToken::new();

    let mut source = YellowstoneSource::new(
        server.url(),
        None,
        "confirmed".to_string(),
        ProgramType::Escrow,
        None,
    )
    .with_gap_detection(rpc_poller, 1_000, 16);

    let handle = source
        .start(tx, cancel.clone())
        .await
        .expect("yellowstone source start");

    // Phase 1: deliver slots 100, 101 pre-disconnect.
    server.enqueue(UpdateMatcher, Update::ok(block_meta(100)));
    server.enqueue(UpdateMatcher, Update::ok(block_meta(101)));

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

    // Phase 2: kill the stream. The source will break out of
    // connect_and_stream with Ok(()), enter the reconnect-gap block, and
    // call RpcPoller to backfill.
    server.drop_stream();

    // Phase 3: enqueue slots 107, 108 so that once the source reconnects
    // post-backfill, streaming resumes cleanly (and we can assert on it).
    server.enqueue(UpdateMatcher, Update::ok(block_meta(107)));
    server.enqueue(UpdateMatcher, Update::ok(block_meta(108)));

    // Collect the remaining SlotCompletes. Expect 102..=106 from backfill
    // plus 107, 108 from the resumed stream.
    let deadline_phase2 = tokio::time::Instant::now() + Duration::from_secs(20);
    let wanted: HashSet<u64> = (102u64..=108u64).collect();
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
