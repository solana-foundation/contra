use clap::Parser;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info};

use contra_auth::config::Config;
use contra_auth::{build_app, db, jwt::JwtConfig, AppState};

/// How often the background task purges expired and used challenge rows.
/// Challenge TTL is 10 minutes, so hourly is more than sufficient.
const CHALLENGE_CLEANUP_INTERVAL: Duration = Duration::from_secs(60 * 60);

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let config = Config::parse();

    info!("Starting contra-auth on port {}", config.port);

    let pool = PgPoolOptions::new()
        .max_connections(config.database_max_connections)
        .connect(&config.database_url)
        .await
        .expect("failed to connect to database");

    info!("Connected to database");

    // Create tables and indexes if they don't exist yet.
    db::init_schema(&pool)
        .await
        .expect("failed to initialize schema");

    info!("Schema initialized");

    let state = AppState {
        pool,
        jwt: Arc::new(JwtConfig::new(&config.jwt_secret)),
    };

    // Periodically remove expired and used challenges so the table doesn't grow unboundedly.
    let cleanup_pool = state.pool.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(CHALLENGE_CLEANUP_INTERVAL).await;
            match db::cleanup_stale_challenges(&cleanup_pool).await {
                Ok(n) => info!(deleted = n, "cleaned up stale challenges"),
                Err(e) => error!("challenge cleanup failed: {e}"),
            }
        }
    });

    let app = build_app(state, &config.cors_allowed_origin);

    let addr = format!("0.0.0.0:{}", config.port);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind");

    axum::serve(listener, app).await.expect("server error");
}
