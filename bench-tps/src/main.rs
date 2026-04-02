//! contra-bench-tps — Contra pipeline load testing binary
//!
//! # Overview
//!
//! The binary supports three subcommands for testing different parts of the Contra pipeline:
//!
//! **`transfer`** (default flow)
//! Tests the L2 SPL transfer pipeline.
//!
//! **`deposit`**
//! Tests the L1 → escrow deposit flow.
//!
//! **`withdraw`**
//! Tests the L2 withdraw-burn flow.
//!
//! Each flow follows the same three-phase structure:
//!
//! **Phase 1 — Setup**
//! Creates all on-chain state required by the load phase.
//!
//! **Phase 2 — Background tasks**
//! Blockhash poller + metrics sampler run concurrently.
//!
//! **Phase 3 — Load**
//! Generator task signs batches; sender threads dispatch them.

mod args;
mod background;
mod bench_metrics;
mod load;
mod load_deposit;
mod load_withdraw;
mod rpc;
mod setup;
mod setup_deposit;
mod setup_withdraw;
mod types;

use {
    anyhow::Result,
    args::{Cli, DerivePdaArgs, SubCommand},
    background::{run_blockhash_poller, run_metrics_sampler, run_operator_mints_sampler},
    bench_metrics::{
        bench_metrics_init, {FLOW_DEPOSIT, FLOW_TRANSFER, FLOW_WITHDRAW},
    },
    clap::Parser,
    contra_core::client::load_keypair,
    load::{build_destinations, run_generator, run_sender_thread},
    load_deposit::{run_deposit_generator, run_deposit_sender_thread},
    load_withdraw::{run_withdraw_generator, run_withdraw_sender_thread},
    serde_json,
    setup_deposit::find_instance_pda,
    solana_sdk::{signature::Keypair, signer::Signer},
    std::{
        collections::VecDeque,
        fs::write,
        sync::{
            atomic::{AtomicU64, Ordering},
            Arc, Condvar, Mutex,
        },
    },
    tokio::time::Duration,
    tokio_util::sync::CancellationToken,
    tracing::info,
    types::{BatchQueue, BenchConfig},
};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.subcommand {
        SubCommand::Transfer(args) => run_transfer(args).await,
        SubCommand::Deposit(args) => run_deposit(args).await,
        SubCommand::Withdraw(args) => run_withdraw(args).await,
        SubCommand::DerivePda(args) => run_derive_pda(args),
    }
}

// ---------------------------------------------------------------------------
// Transfer subcommand
// ---------------------------------------------------------------------------

async fn run_transfer(args: args::TransferArgs) -> Result<()> {
    init_logging(&args.log_level);
    bench_metrics_init();
    if let Some(port) = args.metrics_port {
        contra_metrics::start_metrics_server(port);
    }

    info!(
        rpc_url = %args.rpc_url,
        accounts = args.accounts,
        threads = args.threads,
        duration = args.duration,
        "Starting contra-bench-tps (transfer)",
    );

    // -------------------------------------------------------------------------
    // Phase 1 — Setup
    //
    // Creates the mint, ATAs, and token balances.  The function blocks until
    // all setup transactions are confirmed on-chain before returning.
    // -------------------------------------------------------------------------
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
    // Split accounts into sender (first half) and receiver (second half) pools.
    // num_conflict_groups controls how many distinct receivers are used;
    // defaults to half of accounts for zero sequencer contention.
    let num_conflict_groups = args.num_conflict_groups.unwrap_or(args.accounts / 2);
    let (senders, destinations) = build_destinations(&setup_result.keypairs, num_conflict_groups);
    let config = Arc::new(BenchConfig {
        mint: setup_result.mint,
        accounts: senders,
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
        FLOW_TRANSFER,
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

    info!(
        duration_secs = args.duration,
        threads = args.threads,
        "Transfer load phase started"
    );
    tokio::time::sleep(Duration::from_secs(args.duration)).await;

    info!("Transfer load phase complete — shutting down");
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
        "Final summary (transfer)",
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Deposit subcommand
// ---------------------------------------------------------------------------

async fn run_deposit(args: args::DepositArgs) -> Result<()> {
    init_logging(&args.log_level);
    bench_metrics_init();
    if let Some(port) = args.metrics_port {
        contra_metrics::start_metrics_server(port);
    }

    info!(
        l1_rpc_url = %args.l1_rpc_url,
        accounts = args.accounts,
        threads = args.threads,
        duration = args.duration,
        "Starting contra-bench-tps (deposit)",
    );

    let deposit_config = setup_deposit::run_setup_deposit_phase(
        &args.l1_rpc_url,
        &args.admin_keypair,
        args.instance_seed_keypair.as_deref(),
        args.accounts,
        args.initial_balance,
    )
    .await?;

    let deposit_config = Arc::new(deposit_config);

    let queue: BatchQueue = Arc::new((Mutex::new(VecDeque::new()), Condvar::new()));

    let cancel = CancellationToken::new();

    let bh_handle = tokio::spawn(run_blockhash_poller(
        args.l1_rpc_url.clone(),
        Arc::clone(&deposit_config.state),
        cancel.clone(),
    ));

    let load_end = tokio::time::Instant::now() + Duration::from_secs(args.duration);
    let metrics_handle = tokio::spawn(run_metrics_sampler(
        args.l1_rpc_url.clone(),
        load_end,
        cancel.clone(),
        FLOW_DEPOSIT,
    ));

    let gen_handle = tokio::spawn(run_deposit_generator(
        Arc::clone(&deposit_config),
        Arc::clone(&deposit_config.state),
        Arc::clone(&queue),
        args.threads,
        cancel.clone(),
    ));

    let sent_count = Arc::new(AtomicU64::new(0));
    let mut sender_handles = Vec::with_capacity(args.threads);
    for _ in 0..args.threads {
        let rpc_url = args.l1_rpc_url.clone();
        let q = Arc::clone(&queue);
        let c = cancel.clone();
        let sc = Arc::clone(&sent_count);
        let ms = args.sender_sleep_ms;
        sender_handles.push(std::thread::spawn(move || {
            run_deposit_sender_thread(rpc_url, q, c, sc, ms)
        }));
    }

    let operator_handle = args.operator_metrics_url.clone().map(|url| {
        tokio::spawn(run_operator_mints_sampler(
            url,
            load_end,
            cancel.clone(),
            "escrow",
        ))
    });

    info!(
        duration_secs = args.duration,
        threads = args.threads,
        "Deposit load phase started"
    );
    tokio::time::sleep(Duration::from_secs(args.duration)).await;

    info!("Deposit load phase complete — shutting down");
    cancel.cancel();
    let (_, cvar) = queue.as_ref();
    cvar.notify_all();

    let _ = gen_handle.await;
    let _ = bh_handle.await;
    let (start_tx_count, end_tx_count) = metrics_handle.await.unwrap_or((0, 0));
    let (start_mints, end_mints) = if let Some(h) = operator_handle {
        h.await.unwrap_or((0, 0))
    } else {
        (0, 0)
    };
    for h in sender_handles {
        let _ = h.join();
    }

    let sent = sent_count.load(Ordering::Relaxed);
    let l1_landed = end_tx_count.saturating_sub(start_tx_count);
    let l2_minted = end_mints.saturating_sub(start_mints);
    let l1_tps = l1_landed as f64 / args.duration as f64;

    if args.operator_metrics_url.is_some() {
        let l2_tps = l2_minted as f64 / args.duration as f64;
        let drop = l1_landed.saturating_sub(l2_minted);
        let drop_rate = if l1_landed > 0 {
            drop as f64 / l1_landed as f64 * 100.0
        } else {
            0.0
        };
        info!(
            duration_secs = args.duration,
            sent,
            l1_landed,
            l2_minted,
            drop,
            drop_rate = format!("{drop_rate:.1}%"),
            l1_tps = format!("{l1_tps:.1}"),
            l2_tps = format!("{l2_tps:.1}"),
            "Final summary (deposit)",
        );
    } else {
        info!(
            duration_secs = args.duration,
            sent,
            l1_landed,
            l1_tps = format!("{l1_tps:.1}"),
            "Final summary (deposit)",
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Withdraw subcommand
// ---------------------------------------------------------------------------

async fn run_withdraw(args: args::WithdrawArgs) -> Result<()> {
    init_logging(&args.log_level);
    bench_metrics_init();
    if let Some(port) = args.metrics_port {
        contra_metrics::start_metrics_server(port);
    }

    info!(
        l1_rpc_url = %args.l1_rpc_url,
        rpc_url = %args.rpc_url,
        accounts = args.accounts,
        threads = args.threads,
        duration = args.duration,
        "Starting contra-bench-tps (withdraw)",
    );

    // Full e2e setup: initialise L1 escrow infrastructure + L2 mint and accounts.
    let withdraw_config = Arc::new(
        setup_withdraw::run_setup_withdraw_phase(
            &args.l1_rpc_url,
            &args.rpc_url,
            &args.admin_keypair,
            args.instance_seed_keypair.as_deref(),
            args.accounts,
            args.initial_balance,
        )
        .await?,
    );

    let queue: BatchQueue = Arc::new((Mutex::new(VecDeque::new()), Condvar::new()));

    let cancel = CancellationToken::new();

    let bh_handle = tokio::spawn(run_blockhash_poller(
        args.rpc_url.clone(),
        Arc::clone(&withdraw_config.state),
        cancel.clone(),
    ));

    let load_end = tokio::time::Instant::now() + Duration::from_secs(args.duration);
    // Measures L2 burn transactions confirmed on the write-node
    let l2_metrics_handle = tokio::spawn(run_metrics_sampler(
        args.rpc_url.clone(),
        load_end,
        cancel.clone(),
        FLOW_WITHDRAW,
    ));
    // Samples contra_operator_mints_sent_total from operator-contra for e2e l1_released count
    let operator_handle = args.operator_metrics_url.clone().map(|url| {
        tokio::spawn(run_operator_mints_sampler(
            url,
            load_end,
            cancel.clone(),
            "withdraw",
        ))
    });

    let gen_handle = tokio::spawn(run_withdraw_generator(
        Arc::clone(&withdraw_config),
        Arc::clone(&withdraw_config.state),
        Arc::clone(&queue),
        args.threads,
        cancel.clone(),
    ));

    let sent_count = Arc::new(AtomicU64::new(0));
    let mut sender_handles = Vec::with_capacity(args.threads);
    for _ in 0..args.threads {
        let rpc_url = args.rpc_url.clone();
        let q = Arc::clone(&queue);
        let c = cancel.clone();
        let sc = Arc::clone(&sent_count);
        let ms = args.sender_sleep_ms;
        sender_handles.push(std::thread::spawn(move || {
            run_withdraw_sender_thread(rpc_url, q, c, sc, ms)
        }));
    }

    info!(
        duration_secs = args.duration,
        threads = args.threads,
        "Withdraw load phase started"
    );
    tokio::time::sleep(Duration::from_secs(args.duration)).await;

    info!("Withdraw load phase complete — shutting down");
    cancel.cancel();
    let (_, cvar) = queue.as_ref();
    cvar.notify_all();

    let _ = gen_handle.await;
    let _ = bh_handle.await;
    let (start_l2_count, end_l2_count) = l2_metrics_handle.await.unwrap_or((0, 0));
    let (start_mints, end_mints) = if let Some(h) = operator_handle {
        h.await.unwrap_or((0, 0))
    } else {
        (0, 0)
    };
    for h in sender_handles {
        let _ = h.join();
    }

    let sent = sent_count.load(Ordering::Relaxed);
    let l2_burned = end_l2_count.saturating_sub(start_l2_count);
    let l2_tps = l2_burned as f64 / args.duration as f64;

    if args.operator_metrics_url.is_some() {
        let l1_released = end_mints.saturating_sub(start_mints);
        let l1_tps = l1_released as f64 / args.duration as f64;
        let drop = l2_burned.saturating_sub(l1_released);
        let drop_rate = if l2_burned > 0 {
            drop as f64 / l2_burned as f64 * 100.0
        } else {
            0.0
        };
        info!(
            duration_secs = args.duration,
            sent,
            l2_burned,
            l1_released,
            drop,
            drop_rate = format!("{drop_rate:.1}%"),
            l2_tps = format!("{l2_tps:.1}"),
            l1_tps = format!("{l1_tps:.1}"),
            "Final summary (withdraw)",
        );
    } else {
        info!(
            duration_secs = args.duration,
            sent,
            l2_burned,
            l2_tps = format!("{l2_tps:.1}"),
            "Final summary (withdraw)",
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// DerivePda subcommand
// ---------------------------------------------------------------------------

/// Derives and prints the escrow instance PDA for a given instance-seed keypair.
///
/// If the keypair file does not exist, a new keypair is generated and saved
/// to the specified path before printing the PDA.  This lets run.sh use a
/// single command to both create the keypair and read the PDA.
fn run_derive_pda(args: DerivePdaArgs) -> Result<()> {
    let keypair: Keypair = if args.instance_seed_keypair.exists() {
        load_keypair(&args.instance_seed_keypair)
            .map_err(|e| anyhow::anyhow!("failed to load instance-seed keypair: {e}"))?
    } else {
        let kp = Keypair::new();
        let bytes = kp.to_bytes();
        let json = serde_json::to_string(&bytes.to_vec())?;
        write(&args.instance_seed_keypair, json)?;
        kp
    };

    let (pda, _) = find_instance_pda(&keypair.pubkey());
    println!("{pda}");
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn init_logging(log_level: &str) {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level)),
        )
        .init();
}
