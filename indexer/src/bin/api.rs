use std::net::SocketAddr;
use std::sync::Arc;

use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use tracing::{info, warn};

use contra_indexer::api::{self, AppState};
use contra_indexer::storage::postgres::PostgresDb;
use contra_indexer::PostgresConfig;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info,contra_indexer=debug".into()),
        )
        .init();

    let database_url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL environment variable required");
    let rpc_url = std::env::var("RPC_URL").expect("RPC_URL environment variable required");
    let escrow_authority_str = std::env::var("ESCROW_AUTHORITY").unwrap_or_default();

    let host = std::env::var("ADMIN_API_HOST").unwrap_or_else(|_| "0.0.0.0".into());
    let port: u16 = std::env::var("ADMIN_API_PORT")
        .unwrap_or_else(|_| "3001".into())
        .parse()
        .expect("ADMIN_API_PORT must be a valid port number");

    let admin_api_token = std::env::var("ADMIN_API_TOKEN").ok();
    let cors_origin = std::env::var("ADMIN_CORS_ORIGIN").ok();

    if admin_api_token.is_none() {
        warn!("ADMIN_API_TOKEN not set — API has no authentication (dev mode)");
    }

    let postgres_config = PostgresConfig {
        database_url,
        max_connections: 5,
    };

    let db = PostgresDb::new(&postgres_config).await?;
    let rpc = Arc::new(RpcClient::new(rpc_url));

    let escrow_authority: Pubkey = if escrow_authority_str.is_empty() {
        warn!(
            "ESCROW_AUTHORITY not set — reconciliation will show 'unknown' for on-chain balances"
        );
        Pubkey::default()
    } else {
        escrow_authority_str
            .parse()
            .expect("ESCROW_AUTHORITY must be a valid Solana public key")
    };

    let state = AppState {
        db,
        rpc,
        escrow_authority,
        admin_api_token,
    };

    let app = api::router(state, cors_origin);

    let addr: SocketAddr = format!("{}:{}", host, port).parse()?;
    info!("Admin API listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
