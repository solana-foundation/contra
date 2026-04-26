//! `test_parallel_svm_burst`
//!
//! Target file: `core/src/vm/gasless_callback.rs` — drives the
//! `SnapshotCallback` impl that's only reachable through the parallel-SVM
//! execution path.
//!
//! The parallel path in `core/src/stages/execution.rs` is gated by
//!     `regular_txs_in_batch >= max_svm_workers * MIN_PARALLEL_BATCH_FACTOR`
//! With the integration `NodeConfig` set to `max_svm_workers = 4` and
//! `MIN_PARALLEL_BATCH_FACTOR = 4`, that's a 16-tx threshold. The
//! conflict-free scheduler splits a batch into ConflictFreeBatches; two
//! transfers that share a writable account (e.g. the same fee-payer)
//! always end up in separate sub-batches. So a naive burst from a single
//! fee-payer produces 20 single-tx ConflictFreeBatches and never crosses
//! the parallel threshold.
//!
//! Strategy: set up 20 distinct fee-payer keypairs (funded once during
//! setup), then burst 20 transfers each signed by a different fee-payer.
//! With no shared writable account across the burst, the scheduler keeps
//! all 20 in a single ConflictFreeBatch → `regular_transactions.len() = 20`
//! → `execute_parallel` → `SnapshotCallback::from_bob` + impl arms fire.

use {
    super::test_context::ContraContext,
    solana_client::{nonblocking::rpc_client::RpcClient, rpc_config::RpcSendTransactionConfig},
    solana_sdk::{
        signature::{Keypair, Signer},
        transaction::Transaction,
    },
    solana_system_interface::instruction as system_instruction,
    std::{sync::Arc, time::Duration},
    tokio::time::sleep,
};

const BURST_SIZE: usize = 20;
const SETTLE_DEADLINE_SECS: u64 = 10;

pub async fn run_parallel_svm_burst_test(ctx: &ContraContext) {
    println!("\n=== Parallel-SVM Burst ===");

    // Step 1: build 20 distinct fee-payer keypairs and fund them, so the
    // burst phase can submit 20 transfers that share NO writable account.
    // The funding is sequential (each fund tx writes the operator's account
    // → can't parallelise) but we only need to do it once.
    let fee_payers: Vec<Keypair> = (0..BURST_SIZE).map(|_| Keypair::new()).collect();
    let recipients: Vec<Keypair> = (0..BURST_SIZE).map(|_| Keypair::new()).collect();

    println!("  → funding {} burst fee-payers", BURST_SIZE);
    for (idx, payer) in fee_payers.iter().enumerate() {
        let blockhash = ctx
            .get_blockhash()
            .await
            .expect("getLatestBlockhash for funding");
        // 1_000_000 lamports — well above tx fee + the 100-200 lamport
        // transfer the burst phase will issue.
        let fund_tx = Transaction::new_signed_with_payer(
            &[system_instruction::transfer(
                &ctx.operator_key.pubkey(),
                &payer.pubkey(),
                1_000_000,
            )],
            Some(&ctx.operator_key.pubkey()),
            &[&ctx.operator_key],
            blockhash,
        );
        let sig = ctx
            .send_and_check(&fund_tx, Duration::from_secs(SETTLE_DEADLINE_SECS))
            .await
            .expect("send_and_check should not error")
            .unwrap_or_else(|| {
                panic!("funding tx {idx} failed to land within {SETTLE_DEADLINE_SECS}s")
            });
        let _ = sig; // landed; nothing else to assert
    }
    println!("  → all {} fee-payers funded", BURST_SIZE);

    // Step 2: build the 20 burst txs — each signed by its own fee-payer so
    // there's no write-conflict between any pair. Distinct recipients avoid
    // accidental read-conflicts too.
    let blockhash = ctx
        .get_blockhash()
        .await
        .expect("getLatestBlockhash for burst");

    let txs: Vec<Transaction> = fee_payers
        .iter()
        .zip(recipients.iter())
        .enumerate()
        .map(|(i, (payer, recipient))| {
            let amount = 100 + i as u64;
            Transaction::new_signed_with_payer(
                &[system_instruction::transfer(
                    &payer.pubkey(),
                    &recipient.pubkey(),
                    amount,
                )],
                Some(&payer.pubkey()),
                &[payer],
                blockhash,
            )
        })
        .collect();

    // Single shared client — its internal reqwest pool keeps one HTTP
    // connection alive across all 20 send_transaction calls. Wrapped in
    // Arc so we can clone into each spawned task without re-creating the
    // underlying HTTP machinery per send.
    let client = Arc::new(RpcClient::new(ctx.write_client.url()));

    // Spawn all 20 send_transaction futures together. tokio::spawn returns
    // immediately, so the first send_transaction is already in flight before
    // the loop schedules the next.
    let mut set = tokio::task::JoinSet::new();
    let send_config = RpcSendTransactionConfig {
        skip_preflight: true,
        ..Default::default()
    };
    for tx in txs {
        let client = client.clone();
        set.spawn(async move {
            client
                .send_transaction_with_config(&tx, send_config)
                .await
                .expect("send_transaction should succeed")
        });
    }

    let mut signatures = Vec::with_capacity(BURST_SIZE);
    while let Some(joined) = set.join_next().await {
        signatures.push(joined.expect("join task"));
    }
    assert_eq!(signatures.len(), BURST_SIZE);
    println!("  → submitted {} concurrent transfers", signatures.len());

    // Poll until every signature lands. Generous deadline because parallel
    // execution timing is variable.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(SETTLE_DEADLINE_SECS);
    let mut landed = 0usize;
    while tokio::time::Instant::now() < deadline && landed < signatures.len() {
        landed = 0;
        for sig in &signatures {
            if ctx
                .get_transaction(sig)
                .await
                .expect("get_transaction should not error")
                .is_some()
            {
                landed += 1;
            }
        }
        if landed < signatures.len() {
            sleep(Duration::from_millis(100)).await;
        }
    }

    assert_eq!(
        landed,
        signatures.len(),
        "expected all {} burst txs to settle within {SETTLE_DEADLINE_SECS}s; only {landed} did.",
        signatures.len(),
    );
    println!("  ✓ all {} burst transfers settled", landed);
    println!("✓ Parallel-SVM burst test passed");
}
