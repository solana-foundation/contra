//! CLI argument definitions for contra-bench-tps.
//!
//! Every argument can also be set via an environment variable (useful when
//! running inside Docker or calling from `run.sh`).  Environment variables
//! follow the `BENCH_` prefix convention.

use {clap::Parser, std::path::PathBuf};

#[derive(Parser, Debug)]
#[command(
    name = "contra-bench-tps",
    about = "Load testing binary for the Contra pipeline"
)]
pub struct Args {
    /// Path to the admin keypair JSON file.
    ///
    /// The admin keypair is used to:
    ///   - initialise the SPL mint
    ///   - create ATAs for each generated account
    ///   - mint initial token balances to each ATA
    ///
    /// Generated automatically by `scripts/run.sh`.
    #[arg(long, env = "BENCH_ADMIN_KEYPAIR")]
    pub admin_keypair: PathBuf,

    /// JSON-RPC endpoint of the contra write-node (or gateway).
    ///
    /// `run.sh` points this at the gateway (`http://localhost:GATEWAY_PORT`)
    /// so that read requests are automatically routed to the read-node.
    #[arg(long, default_value = "http://localhost:8899", env = "BENCH_RPC_URL")]
    pub rpc_url: String,

    /// Number of funded source accounts to generate.
    ///
    /// Each account gets its own keypair, ATA, and initial token balance.
    /// Must be >= `--threads` to avoid multiple senders sharing a keypair
    /// (which would cause nonce conflicts).
    #[arg(long, default_value_t = 50, env = "BENCH_ACCOUNTS")]
    pub accounts: usize,

    /// Duration of the load phase in seconds.
    #[arg(long, default_value_t = 60, env = "BENCH_DURATION")]
    pub duration: u64,

    /// Number of concurrent sender threads.
    ///
    /// Each sender thread runs a blocking loop: pop batch → send each tx →
    /// sleep `--sender-sleep-ms` → repeat.  More threads = higher throughput
    /// up to the point where the node or network becomes the bottleneck.
    #[arg(long, default_value_t = 4, env = "BENCH_THREADS")]
    pub threads: usize,

    /// Number of distinct destination accounts.
    ///
    /// Controls how much sequencer contention the test generates:
    ///   - 1            → all senders write to the same destination ATA
    ///     (maximum conflict, stresses the sequencer)
    ///   - == accounts  → each sender has a unique destination
    ///     (no conflicts, maximum throughput)
    ///
    /// Defaults to `--accounts` when omitted.
    #[arg(long, env = "BENCH_NUM_CONFLICT_GROUPS")]
    pub num_conflict_groups: Option<usize>,

    /// Initial token balance (raw units) minted to each account's ATA.
    ///
    /// Each transfer costs 1 raw unit, so this is effectively the number of
    /// transfers an account can make before its balance is exhausted.
    #[arg(long, default_value_t = 1_000_000, env = "BENCH_INITIAL_BALANCE")]
    pub initial_balance: u64,

    /// Optional port for a Prometheus `/metrics` endpoint.
    ///
    /// When set, the binary exposes real-time bench metrics for scraping.
    /// When absent, metrics are written to stdout via `tracing` only.
    #[arg(long, env = "BENCH_METRICS_PORT")]
    pub metrics_port: Option<u16>,

    /// Milliseconds each sender thread sleeps after dispatching one batch.
    ///
    /// Use this to throttle the send rate without reducing `--threads`.
    /// A value of 0 disables the sleep entirely (maximum throughput mode).
    #[arg(long, default_value_t = 5, env = "BENCH_SENDER_SLEEP_MS")]
    pub sender_sleep_ms: u64,

    /// Tracing log level.  One of: error, warn, info, debug, trace.
    ///
    /// The `RUST_LOG` environment variable takes precedence when set.
    #[arg(long, default_value = "info", env = "BENCH_LOG_LEVEL")]
    pub log_level: String,
}
