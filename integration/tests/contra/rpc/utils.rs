use {
    anyhow::Result,
    contra_core::nodes::node::{run_node, NodeConfig, NodeHandles},
    std::{sync::Once, time::Duration},
    tokio::time::sleep,
};

// Ensure tracing is only initialized once across all tests
static INIT: Once = Once::new();

pub const MINT_DECIMALS: u8 = 3;
pub const SEND_AND_CHECK_DURATION_SECONDS: u64 = 1;
pub const LAMPORTS_PER_SOL: u64 = 1_000_000_000;
pub const AIRDROP_LAMPORTS: u64 = LAMPORTS_PER_SOL;

pub fn init_tracing() {
    use tracing_subscriber::{filter::EnvFilter, fmt, prelude::*};

    INIT.call_once(|| {
        let filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("info"))
            .add_directive("solana=off".parse().unwrap())
            .add_directive("agave=off".parse().unwrap());

        tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer())
            .init();
    });
}

/// Start a node with the given configuration
pub async fn start_contra(config: NodeConfig) -> Result<(NodeHandles, String)> {
    let port = config.port;
    let node_handles = run_node(config)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to start node: {}", e))?;
    sleep(Duration::from_secs(1)).await;

    let url = format!("http://127.0.0.1:{}", port);
    println!("\n=== Node Started ===");
    println!("Node endpoint: {}", url);

    Ok((node_handles, url))
}
