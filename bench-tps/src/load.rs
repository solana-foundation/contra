//! Phase 3 — Load generation
//!
//! This module contains the two active components of the load phase:
//!
//! **Generator task** (`run_generator`)
//! A single async task that runs on the tokio runtime.  It reads the current
//! blockhash from `BenchState`, signs a batch of SPL transfer transactions
//! (cycling through `BenchConfig::accounts` and `BenchConfig::destinations`),
//! then pushes the completed batch onto `BatchQueue` and notifies the condvar.
//! It applies backpressure by yielding when the queue is already at
//! `MAX_QUEUE_DEPTH` to avoid unbounded memory growth.
//!
//! **Sender threads** (`run_sender_thread`)
//! `--threads` OS threads, each running a blocking loop.  A sender waits on
//! the condvar until a batch is available, pops it, and calls the *synchronous*
//! `solana_client::rpc_client::RpcClient::send_transaction` for each transaction
//! in the batch.  Using blocking threads instead of async tasks prevents the
//! RPC calls from monopolising the tokio thread pool and allows straightforward
//! backpressure via the condvar.  After each batch the thread increments
//! `sent_count` (by the number of transactions sent) and sleeps for `sleep_ms`
//! if throttling is enabled.

use {
    crate::{
        bench_metrics::{BENCH_SENT_TOTAL, NO_LABELS},
        types::{BatchQueue, BenchConfig, BenchState, MAX_QUEUE_DEPTH, TRANSFER_AMOUNT},
    },
    solana_sdk::{
        hash::Hash, instruction::Instruction, pubkey::Pubkey, signature::Keypair, signer::Signer,
        transaction::Transaction,
    },
    spl_associated_token_account::get_associated_token_address,
    std::sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    tokio_util::sync::CancellationToken,
    tracing::warn,
};

/// SPL Memo v1 program — accepts arbitrary UTF-8 (or raw bytes) as instruction
/// data and succeeds unconditionally, making it the standard way to embed
/// unique metadata in a Solana transaction without affecting token balances.
const MEMO_PROGRAM_ID: Pubkey = solana_sdk::pubkey!("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr");

/// Build a signed SPL token transfer transaction with a memo instruction that
/// encodes `nonce` as 8 little-endian bytes.
///
/// Appending a unique nonce guarantees that every transaction has distinct
/// bytes — and therefore a distinct signature — regardless of whether the
/// `(src, dst, amount, blockhash)` tuple repeats.  This completely eliminates
/// the duplicate-signature rejections that the dedup stage would otherwise
/// produce when accounts or destinations are few relative to batch size.
fn build_transfer(
    from: &Keypair,
    to: &Pubkey,
    mint: &Pubkey,
    amount: u64,
    blockhash: Hash,
    nonce: u64,
) -> Transaction {
    let from_pubkey = from.pubkey();
    let from_ata = get_associated_token_address(&from_pubkey, mint);
    let to_ata = get_associated_token_address(to, mint);

    let transfer_ix = spl_token::instruction::transfer(
        &spl_token::id(),
        &from_ata,
        &to_ata,
        &from_pubkey,
        &[],
        amount,
    )
    .unwrap();

    // Memo carries the 8-byte little-endian encoding of `nonce` (= tx_seq).
    // The memo program requires no accounts and accepts any byte sequence.
    let memo_ix = Instruction {
        program_id: MEMO_PROGRAM_ID,
        accounts: vec![],
        data: nonce.to_le_bytes().to_vec(),
    };

    Transaction::new_signed_with_payer(
        &[transfer_ix, memo_ix],
        Some(&from_pubkey),
        &[from],
        blockhash,
    )
}

/// Split funded accounts into sender and receiver pools for the load phase.
///
/// The accounts list is divided in half:
///   - **Senders**   (`accounts[0..n/2]`) — sign and pay for each transaction.
///   - **Receivers** (`accounts[n/2..n]`) — receive tokens; their pubkeys are returned.
///
/// No account appears in both roles, so no two concurrent transactions share
/// an account and sequencer contention is zero at the default setting.
///
/// `num_conflict_groups` controls how many distinct receiver accounts are used
/// (clamped to the size of the receiver pool):
///   - 1         → every sender targets the same receiver (maximum contention)
///   - pool size → each sender has a unique receiver (zero contention)
///
/// Unlike a self-transfer (`src == dst`), each transaction produces a real
/// balance change.  Senders drain at `TRANSFER_AMOUNT` per transaction, but
/// with `--initial-balance 1_000_000` and `TRANSFER_AMOUNT = 1` there is
/// ample runway for any typical bench run.
pub fn build_destinations(
    accounts: &[Arc<solana_sdk::signature::Keypair>],
    num_conflict_groups: usize,
) -> (Vec<Arc<solana_sdk::signature::Keypair>>, Vec<Pubkey>) {
    let mid = accounts.len() / 2;
    let senders = accounts[..mid].to_vec();
    let n = num_conflict_groups.min(mid).max(1);
    let receivers = accounts[mid..mid + n]
        .iter()
        .map(|kp| kp.pubkey())
        .collect();
    (senders, receivers)
}

/// Async generator task: signs batches of SPL transfer transactions and pushes
/// them onto `queue` for sender threads to consume.
///
/// The generator cycles through `config.accounts` and `config.destinations`
/// using a wrapping sequence counter so that no two consecutive batches use
/// the same (source, destination) pair (assuming accounts > 1).
///
/// Exits when `cancel` is triggered.
pub async fn run_generator(
    config: Arc<BenchConfig>,
    state: Arc<BenchState>,
    queue: BatchQueue,
    batch_size: usize,
    cancel: CancellationToken,
) {
    // Monotonically increasing counter used to index into accounts/destinations.
    // Wraps at usize::MAX without panic.
    let mut tx_seq: usize = 0;

    loop {
        if cancel.is_cancelled() {
            break;
        }

        // Backpressure: if the queue is full the senders are the bottleneck.
        // Yield to the tokio scheduler and check again on the next turn rather
        // than spinning or sleeping.
        {
            let (lock, _) = queue.as_ref();
            if lock.lock().unwrap().len() >= MAX_QUEUE_DEPTH {
                tokio::task::yield_now().await;
                continue;
            }
        }

        // Read the latest blockhash.  The blockhash poller keeps this fresh
        // so that the signed transactions are not rejected for a stale hash.
        let blockhash = *state.current_blockhash.read().await;

        let mut batch = Vec::with_capacity(batch_size);
        for _ in 0..batch_size {
            let src = &config.accounts[tx_seq % config.accounts.len()];
            let dst = &config.destinations[tx_seq % config.destinations.len()];
            // tx_seq is passed as the memo nonce so every transaction has a
            // unique signature regardless of blockhash or account cycling.
            let tx = build_transfer(
                src,
                dst,
                &config.mint,
                TRANSFER_AMOUNT,
                blockhash,
                tx_seq as u64,
            );
            batch.push(tx);
            tx_seq = tx_seq.wrapping_add(1);
        }

        // Push the batch and wake one waiting sender thread.
        let (lock, cvar) = queue.as_ref();
        lock.lock().unwrap().push_back(batch);
        cvar.notify_one();

        // Yield after each batch so the blockhash poller and metrics sampler
        // stay responsive on the same tokio thread.
        tokio::task::yield_now().await;
    }
}

/// Blocking sender thread: pops one batch at a time and sends each transaction
/// via the synchronous (blocking) `RpcClient`.
///
/// The condvar wait uses a 50 ms timeout so that a cancellation signal is
/// checked at least every 50 ms even when the queue is idle.
///
/// `sent_count` is incremented by `batch.len()` (not by 1) so the counter
/// reflects individual transactions, not batches.
pub fn run_sender_thread(
    rpc_url: String,
    queue: BatchQueue,
    cancel: CancellationToken,
    sent_count: Arc<AtomicU64>,
    sleep_ms: u64,
) {
    // Each sender thread owns its own blocking RpcClient so there is no lock
    // contention between threads on the connection pool.
    let rpc = solana_client::rpc_client::RpcClient::new(rpc_url);

    loop {
        if cancel.is_cancelled() {
            break;
        }

        // Block until a batch is available or a 50 ms timeout elapses.
        // The timeout ensures we re-check `cancel` even when the queue is idle.
        let batch = {
            let (lock, cvar) = queue.as_ref();
            let mut q = lock.lock().unwrap();
            loop {
                if cancel.is_cancelled() {
                    return;
                }
                if let Some(batch) = q.pop_front() {
                    break batch;
                }
                let (new_q, _) = cvar
                    .wait_timeout(q, std::time::Duration::from_millis(50))
                    .unwrap();
                q = new_q;
            }
        };

        // Send each transaction in the batch sequentially.  A synchronous call
        // here is intentional: it naturally throttles the sender to the round-
        // trip time of one HTTP request, giving the generator time to pre-sign
        // the next batch while this one is in flight.
        for tx in &batch {
            BENCH_SENT_TOTAL.with_label_values(&NO_LABELS).inc();
            match rpc.send_transaction(tx) {
                Ok(_) => {}
                Err(e) => warn!(err = %e, "sender: send_transaction failed"),
            }
        }

        // Record the number of transactions dispatched in this batch.
        sent_count.fetch_add(batch.len() as u64, Ordering::Relaxed);

        // Optional throttle: a non-zero sleep_ms limits the peak send rate
        // without reducing the number of sender threads.
        if sleep_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
        }
    }
}
