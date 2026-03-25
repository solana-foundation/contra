//! Shared types and constants used across all phases of the bench.
//!
//! Keeping these in one place makes it easy to tune the bench without hunting
//! through multiple files.

use {
    solana_sdk::{hash::Hash, pubkey::Pubkey, signature::Keypair, transaction::Transaction},
    std::{
        collections::VecDeque,
        sync::{Arc, Condvar, Mutex},
    },
    tokio::{sync::RwLock, time::Duration},
};

// ---------------------------------------------------------------------------
// Tuning constants
// ---------------------------------------------------------------------------

/// Number of decimal places for the SPL mint created during setup.
pub const MINT_DECIMALS: u8 = 6;

/// Maximum time to wait for a batch of on-chain confirmations before giving up.
pub const CONFIRM_TIMEOUT: Duration = Duration::from_secs(120);

/// Delay between successive `getSignatureStatuses` polls during confirmation.
pub const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Maximum number of transactions sent concurrently per ATA/mint-to batch
/// during the setup phase.  Keeps the in-flight HTTP connection count bounded.
pub const MAX_CONCURRENT_SENDS: usize = 64;

/// How often the background blockhash poller calls `getLatestBlockhash`.
/// The contra node rejects transactions whose blockhash is older than ~15 s,
/// so refreshing at 80 ms gives a comfortable margin.
pub const BLOCKHASH_POLL_INTERVAL: Duration = Duration::from_millis(80);

/// How often the metrics sampler fires to log instantaneous TPS.
pub const METRICS_SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

/// How often the blockhash poller emits an average fetch-latency log line.
pub const BLOCKHASH_LOG_INTERVAL: Duration = Duration::from_secs(2);

/// Base token units transferred per SPL transfer transaction.
/// 1 raw unit = 0.000001 token (6 decimals).
pub const TRANSFER_AMOUNT: u64 = 1;

/// The actual transfer amount is `TRANSFER_AMOUNT + (tx_seq % AMOUNT_VARIANCE)`,
/// giving amounts in the range [1, 100].  This ensures that transactions with
/// the same (src, dst, blockhash) — which can repeat when accounts < batch_size
/// — produce distinct byte payloads and therefore distinct signatures, so the
/// node's dedup stage cannot treat them as duplicates.
///
/// With `--initial-balance 1_000_000` each account can still make at least
/// 1_000_000 / 100 = 10_000 transfers before exhaustion.
pub const AMOUNT_VARIANCE: u64 = 100;

/// Maximum number of pending batches allowed in the queue before the
/// generator yields.  This bounds queue memory and prevents the generator
/// from running too far ahead of the senders.
pub const MAX_QUEUE_DEPTH: usize = 32;

/// Number of accounts processed per setup batch (ATA creation, mint-to).
/// After each batch all transactions are confirmed before the next batch
/// starts, and only the ones that failed to land are retried.
pub const SETUP_BATCH_SIZE: usize = 200;

/// Maximum signatures per `getSignatureStatuses` RPC call.
/// The Solana RPC spec caps this at 256; exceeding it returns HTTP 413.
pub const SIG_STATUS_CHUNK_SIZE: usize = 256;

// ---------------------------------------------------------------------------
// Shared data structures
// ---------------------------------------------------------------------------

/// Immutable configuration for the load phase, shared across the generator
/// task and all sender threads via `Arc`.
pub struct BenchConfig {
    /// The SPL mint created during setup.  All ATAs and transfers use this mint.
    pub mint: Pubkey,

    /// Source keypairs generated during setup, one per funded account.
    /// The generator cycles through these round-robin so that each keypair
    /// signs roughly the same number of transactions.
    pub accounts: Vec<Arc<Keypair>>,

    /// Destination wallet pubkeys, len = num_conflict_groups.
    /// `create_spl_transfer` derives the destination ATA from each pubkey
    /// internally, so these are owner addresses, not ATA addresses.
    ///
    /// Setting num_conflict_groups = 1 forces all transfers to the same
    /// destination, which maximises sequencer contention (a single conflict
    /// chain).  Setting it to `accounts` gives each sender a unique
    /// destination and eliminates sequencer conflicts entirely.
    pub destinations: Vec<Pubkey>,
}

/// Mutable shared state updated by background tasks and read by the generator.
pub struct BenchState {
    /// The most recently fetched blockhash.  The blockhash poller writes here
    /// every 80 ms; the generator reads it before signing each batch.
    /// Using a `RwLock` allows many concurrent readers with rare writes.
    pub current_blockhash: RwLock<Hash>,
}

/// The shared batch queue between the async generator task and the blocking
/// sender threads.
///
/// Inner type: `(Mutex<VecDeque<Vec<Transaction>>>, Condvar)`
///
/// - The `Mutex` guards the deque.
/// - The `Condvar` lets sender threads block cheaply when the queue is empty
///   instead of spinning, and wakes them when the generator pushes a new batch.
pub type BatchQueue = Arc<(Mutex<VecDeque<Vec<Transaction>>>, Condvar)>;
