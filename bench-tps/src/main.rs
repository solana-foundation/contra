//! contra-bench-tps — Contra pipeline load testing binary
//!
//! # Overview
//!
//! The binary runs in three sequential phases:
//!
//! **Phase 1 — Setup** (`setup` module)
//! Creates all on-chain state required by the load phase: a fresh SPL mint,
//! Associated Token Accounts for every generated keypair, and an initial token
//! balance for each account.  All setup transactions use fire-and-forget
//! `send_transaction` followed by a custom `poll_confirmations` loop because
//! the contra node's asynchronous pipeline outlasts the blockhash-expiry
//! timeout baked into `send_and_confirm_transaction`.
//!
//! **Phase 2 — Background tasks** (`background` module)
//! Two tasks run concurrently for the entire load phase:
//!   - *Blockhash poller*: refreshes `BenchState::current_blockhash` every
//!     80 ms so the generator always signs with a recent hash.
//!   - *Metrics sampler*: calls `getTransactionCount` every second, logs
//!     instantaneous TPS and remaining test time, and returns the start/end
//!     counts for the final summary.
//!
//! **Phase 3 — Load** (`load` module)
//! A single async generator task signs batches of SPL transfer transactions
//! and pushes them onto a `BatchQueue`.  A pool of `--threads` OS threads each
//! pop one batch at a time and send every transaction via a synchronous
//! `RpcClient`.  A shared `AtomicU64` tracks total transactions sent so the
//! final summary can compute the drop rate against the node's own transaction
//! counter.

mod args;
mod background;
mod load;
mod rpc;
mod setup;
mod types;

use {
    anyhow::Result,
    args::Args,
    background::{run_blockhash_poller, run_metrics_sampler},
    clap::Parser,
    load::{build_destinations, run_generator, run_sender_thread},
    std::sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    tokio::time::Duration,
    tokio_util::sync::CancellationToken,
    tracing::info,
    types::{BatchQueue, BenchConfig},
};

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&args.log_level)),
        )
        .init();

    info!(
        rpc_url = %args.rpc_url,
        accounts = args.accounts,
        threads = args.threads,
        duration = args.duration,
        "Starting contra-bench-tps",
    );

    // -------------------------------------------------------------------------
    // Phase 1 — Setup
    //
    // Creates the mint, ATAs, and token balances.  The function blocks until
    // all setup transactions are confirmed on-chain before returning.
    // -------------------------------------------------------------------------
    let num_conflict_groups = args.num_conflict_groups.unwrap_or(args.accounts);
    let setup_result = setup::run_setup_phase(
        &args.rpc_url,
        &args.admin_keypair,
        args.accounts,
        args.initial_balance,
    )
    .await?;

    // -------------------------------------------------------------------------
    // Phase 2 + 3 — Background tasks and load generation
    //
    // Build the shared config and queue, then spawn background tasks (blockhash
    // poller, metrics sampler) and the generator + sender threads concurrently.
    // The main task waits for `args.duration` seconds, then cancels everything.
    // -------------------------------------------------------------------------
    let destinations = build_destinations(&setup_result.keypairs, num_conflict_groups);
    let config = Arc::new(BenchConfig {
        mint: setup_result.mint,
        accounts: setup_result.keypairs,
        destinations,
    });

    // The queue is an Arc-wrapped (Mutex<VecDeque>, Condvar) pair.  The
    // generator pushes onto it from an async context; sender threads pop from
    // it in a blocking context using the condvar for efficient waiting.
    let queue: BatchQueue = Arc::new((
        std::sync::Mutex::new(std::collections::VecDeque::new()),
        std::sync::Condvar::new(),
    ));

    let cancel = CancellationToken::new();

    // Blockhash poller: keeps BenchState::current_blockhash fresh.
    let bh_handle = tokio::spawn(run_blockhash_poller(
        args.rpc_url.clone(),
        Arc::clone(&setup_result.state),
        cancel.clone(),
    ));

    // Metrics sampler: logs instantaneous TPS every second, returns
    // (start_tx_count, end_tx_count) for the final drop-rate calculation.
    let load_end = tokio::time::Instant::now() + Duration::from_secs(args.duration);
    let metrics_handle = tokio::spawn(run_metrics_sampler(
        args.rpc_url.clone(),
        load_end,
        cancel.clone(),
    ));

    // Generator: signs batches of `threads` transactions and enqueues them.
    let gen_handle = tokio::spawn(run_generator(
        Arc::clone(&config),
        Arc::clone(&setup_result.state),
        Arc::clone(&queue),
        args.threads,
        cancel.clone(),
    ));

    // Sender threads: each pops one batch and calls send_transaction for every
    // transaction in the batch using the blocking RPC client.
    let sent_count = Arc::new(AtomicU64::new(0));
    let mut sender_handles = Vec::with_capacity(args.threads);
    for _ in 0..args.threads {
        let rpc_url = args.rpc_url.clone();
        let q = Arc::clone(&queue);
        let c = cancel.clone();
        let sc = Arc::clone(&sent_count);
        sender_handles.push(std::thread::spawn(move || {
            run_sender_thread(rpc_url, q, c, sc, args.sender_sleep_ms)
        }));
    }

    info!(duration_secs = args.duration, threads = args.threads, "Load phase started");
    tokio::time::sleep(Duration::from_secs(args.duration)).await;

    // Cancel all background tasks and wake any sender threads parked on the
    // condvar so they observe the cancellation without waiting up to 50 ms.
    info!("Load phase complete — shutting down");
    cancel.cancel();
    let (_, cvar) = queue.as_ref();
    cvar.notify_all();

    // Await async tasks first, then join OS threads.
    let _ = gen_handle.await;
    let _ = bh_handle.await;
    let (start_tx_count, end_tx_count) = metrics_handle.await.unwrap_or((0, 0));
    for h in sender_handles {
        let _ = h.join();
    }

    // -------------------------------------------------------------------------
    // Final summary
    //
    // `sent`   — transactions dispatched by sender threads (from AtomicU64)
    // `landed` — transactions confirmed by the node (from getTransactionCount)
    // `dropped`— sent - landed (rejected by dedup / sigverify / sequencer)
    // -------------------------------------------------------------------------
    let sent = sent_count.load(Ordering::Relaxed);
    let landed = end_tx_count.saturating_sub(start_tx_count);
    let dropped = sent.saturating_sub(landed);
    let drop_rate = if sent > 0 {
        dropped as f64 / sent as f64 * 100.0
    } else {
        0.0
    };
    let tps = landed as f64 / args.duration as f64;
    info!(
        duration_secs = args.duration,
        sent,
        landed,
        dropped,
        drop_rate = format!("{drop_rate:.1}%"),
        tps = format!("{tps:.1}"),
        "Final summary",
    );

    Ok(())
}
