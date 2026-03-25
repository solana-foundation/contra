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
        types::{
            BatchQueue, BenchConfig, BenchState, AMOUNT_VARIANCE, MAX_QUEUE_DEPTH, TRANSFER_AMOUNT,
        },
    },
    contra_core::client::create_spl_transfer,
    solana_sdk::pubkey::Pubkey,
    std::sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    tokio_util::sync::CancellationToken,
    tracing::warn,
};

/// Build the list of destination wallet pubkeys for the load phase.
///
/// Takes the first `n` accounts from the funded keypair list and returns their
/// public keys.  `create_spl_transfer` derives ATAs from these pubkeys
/// internally, so no extra on-chain lookup is needed.
///
/// The caller passes `num_conflict_groups` as `n`:
///   - n = 1         → single destination, maximum sequencer contention
///   - n = accounts  → unique destination per account, no sequencer contention
pub fn build_destinations(accounts: &[Arc<solana_sdk::signature::Keypair>], n: usize) -> Vec<Pubkey> {
    accounts.iter().take(n).map(|kp| {
        use solana_sdk::signer::Signer;
        kp.pubkey()
    }).collect()
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
            let amount = TRANSFER_AMOUNT + (tx_seq as u64 % AMOUNT_VARIANCE);
            let tx = create_spl_transfer(src, dst, &config.mint, amount, blockhash);
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
                let (new_q, _) =
                    cvar.wait_timeout(q, std::time::Duration::from_millis(50)).unwrap();
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
