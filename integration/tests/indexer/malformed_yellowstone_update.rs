//! YellowstoneSource defensive branches: malformed-update handling.
//!
//! Exercises the error paths in
//! `indexer/src/indexer/datasource/yellowstone/source.rs` that only fire on a
//! malformed upstream `SubscribeUpdate`. All assertions pin the CURRENT
//! production contract; behaviour changes should fail these tests.
//!
//! ## Pinned contract (per source.rs, 2026-04)
//!
//! 1. Stream-level error (`tonic::Status`) ⇒ `connect_and_stream` returns
//!    `Err(DataSourceRpcError::Protocol)`, the outer loop logs via `error!`,
//!    increments `INDEXER_DATASOURCE_RECONNECTS` + `INDEXER_RPC_ERRORS`,
//!    sleeps 5s, and reconnects.
//! 2. `SubscribeUpdateTransaction` with missing inner `transaction` /
//!    `message` ⇒ `handle_transaction` returns `Err(...)`, which
//!    `connect_and_stream` propagates as `Err(DataSourceError::Rpc)`.
//!    Source treats this as a stream-level protocol error and reconnects.
//!    (Per source.rs comments this is the "category b" defensive branch —
//!    a single malformed transaction kills the live subscription. This
//!    behaviour is under-cautious but IS the current contract; flagging
//!    for follow-up in the commit message rather than fixing here.)
//! 3. Transaction whose compiled instructions reference a different program
//!    ID ⇒ inner-loop `continue`, no stream-level impact, no error, no
//!    reconnect; subsequent updates on the same stream keep flowing.

use private_channel_indexer::config::ProgramType;
use private_channel_indexer::indexer::datasource::common::datasource::DataSource;
use private_channel_indexer::indexer::datasource::common::parser::escrow::PRIVATE_CHANNEL_ESCROW_PROGRAM_ID;
use private_channel_indexer::indexer::datasource::common::types::ProcessorMessage;
use private_channel_indexer::indexer::datasource::yellowstone::YellowstoneSource;
use std::str::FromStr;
use std::time::Duration;
use test_utils::mock_yellowstone::{MockYellowstoneServer, Update, UpdateMatcher};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use yellowstone_grpc_proto::geyser::{
    subscribe_update::UpdateOneof, SubscribeUpdate, SubscribeUpdateBlockMeta,
    SubscribeUpdateTransaction, SubscribeUpdateTransactionInfo,
};
use yellowstone_grpc_proto::solana::storage::confirmed_block::{
    CompiledInstruction as ProtoCompiledInstruction, Message as ProtoMessage, MessageHeader,
    Transaction as ProtoTransaction,
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

/// Transaction update referencing `program_id_override` instead of the escrow
/// program — mimics a gRPC filter leak that lets a stray program through.
fn tx_update_with_program(slot: u64, program_id: solana_sdk::pubkey::Pubkey) -> SubscribeUpdate {
    let mut account_keys: Vec<Vec<u8>> = (0..12)
        .map(|i| {
            let mut bytes = [0u8; 32];
            bytes[0] = (i + 1) as u8;
            bytes.to_vec()
        })
        .collect();
    account_keys.push(program_id.to_bytes().to_vec());

    let mut ix_data = vec![6u8];
    ix_data.extend_from_slice(&1_000u64.to_le_bytes());
    ix_data.push(0u8);

    let instruction = ProtoCompiledInstruction {
        program_id_index: 12,
        accounts: (0u8..12).collect(),
        data: ix_data,
    };

    let message = ProtoMessage {
        header: Some(MessageHeader {
            num_required_signatures: 1,
            num_readonly_signed_accounts: 0,
            num_readonly_unsigned_accounts: 1,
        }),
        account_keys,
        recent_blockhash: vec![0u8; 32],
        instructions: vec![instruction],
        versioned: false,
        address_table_lookups: vec![],
    };

    let transaction = ProtoTransaction {
        signatures: vec![vec![7u8; 64]],
        message: Some(message),
    };

    SubscribeUpdate {
        filters: vec!["private_channel_program".to_string()],
        update_oneof: Some(UpdateOneof::Transaction(SubscribeUpdateTransaction {
            transaction: Some(SubscribeUpdateTransactionInfo {
                signature: vec![7u8; 64],
                is_vote: false,
                transaction: Some(transaction),
                meta: None,
                index: 0,
            }),
            slot,
        })),
        created_at: None,
    }
}

/// Transaction update whose `program_id_index` exceeds the account-keys
/// length. The defensive bounds check in the Yellowstone source's
/// transaction-update handler logs + continues without forwarding the
/// instruction.
fn tx_update_bad_program_index(slot: u64) -> SubscribeUpdate {
    // Only 3 account keys but instruction points to index 99 — out of bounds.
    let account_keys: Vec<Vec<u8>> = (0..3)
        .map(|i| {
            let mut bytes = [0u8; 32];
            bytes[0] = (i + 1) as u8;
            bytes.to_vec()
        })
        .collect();

    let instruction = ProtoCompiledInstruction {
        program_id_index: 99, // ← out of range for account_keys.len() == 3
        accounts: vec![0u8, 1, 2],
        data: vec![0x00],
    };
    let message = ProtoMessage {
        header: Some(MessageHeader {
            num_required_signatures: 1,
            num_readonly_signed_accounts: 0,
            num_readonly_unsigned_accounts: 1,
        }),
        account_keys,
        recent_blockhash: vec![0u8; 32],
        instructions: vec![instruction],
        versioned: false,
        address_table_lookups: vec![],
    };
    let transaction = ProtoTransaction {
        signatures: vec![vec![0x77u8; 64]],
        message: Some(message),
    };
    SubscribeUpdate {
        filters: vec!["private_channel_program".to_string()],
        update_oneof: Some(UpdateOneof::Transaction(SubscribeUpdateTransaction {
            transaction: Some(SubscribeUpdateTransactionInfo {
                signature: vec![0x77u8; 64],
                is_vote: false,
                transaction: Some(transaction),
                meta: None,
                index: 0,
            }),
            slot,
        })),
        created_at: None,
    }
}

/// Transaction update missing the inner `transaction.message` — exercises the
/// `Missing message` defensive branch in `handle_transaction`.
fn tx_update_missing_message(slot: u64) -> SubscribeUpdate {
    let transaction = ProtoTransaction {
        signatures: vec![vec![0x55; 64]],
        message: None,
    };

    SubscribeUpdate {
        filters: vec!["private_channel_program".to_string()],
        update_oneof: Some(UpdateOneof::Transaction(SubscribeUpdateTransaction {
            transaction: Some(SubscribeUpdateTransactionInfo {
                signature: vec![0x55; 64],
                is_vote: false,
                transaction: Some(transaction),
                meta: None,
                index: 0,
            }),
            slot,
        })),
        created_at: None,
    }
}

struct TestHarness {
    server: MockYellowstoneServer,
    rx: mpsc::Receiver<ProcessorMessage>,
    cancel: CancellationToken,
    handle: tokio::task::JoinHandle<()>,
}

async fn spin_up() -> TestHarness {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info,private_channel_indexer=debug")
        .with_test_writer()
        .try_init();

    let server = MockYellowstoneServer::start().await;
    let (tx, rx) = mpsc::channel::<ProcessorMessage>(64);
    let cancel = CancellationToken::new();

    let mut source = YellowstoneSource::new(
        server.url(),
        None,
        "confirmed".to_string(),
        ProgramType::Escrow,
        None,
    );
    let handle = source
        .start(tx, cancel.clone())
        .await
        .expect("yellowstone source start");

    TestHarness {
        server,
        rx,
        cancel,
        handle,
    }
}

async fn tear_down(h: TestHarness) {
    h.cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(3), h.handle).await;
    h.server.shutdown().await;
}

/// Drain any messages currently pending on the channel within `window`,
/// collecting only `SlotComplete` slots.
async fn drain_slots(rx: &mut mpsc::Receiver<ProcessorMessage>, window: Duration) -> Vec<u64> {
    let mut slots = vec![];
    let deadline = tokio::time::Instant::now() + window;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(ProcessorMessage::SlotComplete { slot, .. })) => slots.push(slot),
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    slots
}

/// Case (a): a `Status::invalid_argument` mid-stream forces a reconnect.
/// The source should open a second `subscribe` RPC and resume delivery.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_error_triggers_reconnect_and_resumes() {
    let mut h = spin_up().await;

    // Deliver one slot, then a malformed stream, then a final slot after
    // reconnect. The 5s sleep inside source.rs's error arm is the
    // dominating factor in this test's runtime.
    h.server.enqueue(UpdateMatcher, Update::ok(block_meta(10)));
    h.server
        .enqueue(UpdateMatcher, Update::malformed("corrupted bytes"));

    // Wait for the first slot and let the malformed update drop the stream.
    let first = tokio::time::timeout(Duration::from_secs(5), h.rx.recv())
        .await
        .expect("timed out waiting for first BlockMeta")
        .expect("channel closed");
    matches!(first, ProcessorMessage::SlotComplete { slot: 10, .. });

    // Wait for the reconnect handshake before enqueuing the follow-up — if
    // we push slot 11 while the first-stream pump is still alive, the pump
    // can dequeue slot 11, try to send on the dead stream, fail, and lose
    // the update entirely.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if h.server.call_count("subscribe") >= 2 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("source should reconnect within 10s of malformed stream error");

    // Now enqueue a follow-up that the RECONNECTED stream should deliver.
    h.server.enqueue(UpdateMatcher, Update::ok(block_meta(11)));

    // Give the source time to hit its 5s reconnect sleep and come back.
    let next_slot = tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            if let Some(msg) = h.rx.recv().await {
                if let ProcessorMessage::SlotComplete { slot, .. } = msg {
                    return Some(slot);
                }
            } else {
                return None;
            }
        }
    })
    .await
    .expect("timed out waiting for post-reconnect delivery")
    .expect("channel closed before reconnect delivered slot 11");

    assert_eq!(
        next_slot, 11,
        "post-reconnect slot should be delivered on the new subscribe stream"
    );
    assert!(
        h.server.call_count("subscribe") >= 2,
        "expected at least 2 subscribe handshakes (original + reconnect), got {}",
        h.server.call_count("subscribe")
    );

    tear_down(h).await;
}

/// Case (b): a transaction with a missing `message` field is treated as a
/// stream-level protocol error (handle_transaction returns `Err`) — the
/// stream terminates and the source reconnects. Subsequent scripted updates
/// are delivered on the new stream.
///
/// Note: a more resilient contract would skip the individual bad tx. Current
/// behaviour surfaces this as a stream-kill — documented above, flagging
/// only — the test pins what the code does today.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_message_kills_stream_and_reconnects() {
    let mut h = spin_up().await;

    h.server.enqueue(UpdateMatcher, Update::ok(block_meta(20)));
    h.server
        .enqueue(UpdateMatcher, Update::ok(tx_update_missing_message(21)));

    let first = tokio::time::timeout(Duration::from_secs(5), h.rx.recv())
        .await
        .expect("timed out waiting for slot 20")
        .expect("channel closed");
    matches!(first, ProcessorMessage::SlotComplete { slot: 20, .. });

    // Wait for the source to actually reconnect before enqueuing the next
    // slot — otherwise the mock's first-stream pump can race ahead and
    // consume slot 22 from the queue before it notices the client closed
    // (the slot would be dequeued, send() would fail, slot is lost).
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if h.server.call_count("subscribe") >= 2 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("source should reconnect within 10s of stream-killing bad tx");

    h.server.enqueue(UpdateMatcher, Update::ok(block_meta(22)));

    let got = tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            if let Some(ProcessorMessage::SlotComplete { slot, .. }) = h.rx.recv().await {
                if slot == 22 {
                    return Some(slot);
                }
            }
        }
    })
    .await
    .expect("timed out waiting for post-reconnect slot 22");

    assert_eq!(got, Some(22));
    assert!(
        h.server.call_count("subscribe") >= 2,
        "malformed tx should have killed + triggered reconnect; saw {} subscribes",
        h.server.call_count("subscribe")
    );

    tear_down(h).await;
}

/// Case (b2): a transaction whose `program_id_index` points past the
/// `account_keys` array hits the defensive `continue` arm in the
/// Yellowstone source's transaction-update handler. The stream must stay
/// healthy; subsequent BlockMetas must flow.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn out_of_bounds_program_id_index_is_silently_skipped() {
    let mut h = spin_up().await;

    h.server.enqueue(UpdateMatcher, Update::ok(block_meta(40)));
    h.server
        .enqueue(UpdateMatcher, Update::ok(tx_update_bad_program_index(41)));
    h.server.enqueue(UpdateMatcher, Update::ok(block_meta(42)));

    let slots = drain_slots(&mut h.rx, Duration::from_secs(4)).await;
    assert_eq!(
        slots,
        vec![40, 42],
        "out-of-bounds program_id_index tx must not produce an Instruction \
         message, and subsequent BlockMeta updates must still flow"
    );
    assert_eq!(
        h.server.call_count("subscribe"),
        1,
        "defensive skip is a soft filter (no reconnect)"
    );

    tear_down(h).await;
}

/// Case (c): a transaction carrying an instruction for an unrelated program
/// is filtered out inside `handle_transaction` (the inner-loop `continue`
/// path). The stream stays alive, no error is logged, and subsequent
/// updates flow normally.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_program_id_is_silently_filtered() {
    let mut h = spin_up().await;

    // An unrelated program ID — use system program (11...11).
    let wrong_program = solana_sdk::pubkey::Pubkey::default();

    h.server.enqueue(UpdateMatcher, Update::ok(block_meta(30)));
    h.server.enqueue(
        UpdateMatcher,
        Update::ok(tx_update_with_program(31, wrong_program)),
    );
    h.server.enqueue(UpdateMatcher, Update::ok(block_meta(32)));

    let slots = drain_slots(&mut h.rx, Duration::from_secs(4)).await;
    assert_eq!(
        slots,
        vec![30, 32],
        "wrong-program-id tx should not produce an Instruction message, and \
         subsequent BlockMeta updates must still flow"
    );
    assert_eq!(
        h.server.call_count("subscribe"),
        1,
        "wrong program id is a soft filter (no reconnect)"
    );

    // Sanity: confirm the escrow program ID is what the source was configured
    // for — the filtered tx legitimately did not match.
    assert_ne!(
        wrong_program,
        solana_sdk::pubkey::Pubkey::from_str(PRIVATE_CHANNEL_ESCROW_PROGRAM_ID).unwrap()
    );

    tear_down(h).await;
}
