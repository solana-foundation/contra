use clap::Parser;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;

use contra_auth::config::Config;
use contra_auth::{build_app, db, jwt::JwtConfig, AppState};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let config = Config::parse();

    tracing::info!("Starting contra-auth on port {}", config.port);

    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&config.database_url)
        .await
        .expect("failed to connect to database");

    tracing::info!("Connected to database");

    // Create tables and indexes if they don't exist yet.
    db::init_schema(&pool)
        .await
        .expect("failed to initialize schema");

    tracing::info!("Schema initialized");

    let state = AppState {
        pool,
        jwt: Arc::new(JwtConfig::new(&config.jwt_secret)),
    };

    let app = build_app(state);

    let addr = format!("0.0.0.0:{}", config.port);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind");

    axum::serve(listener, app).await.expect("server error");
}
