pub mod routes;
pub mod types;

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::Response,
    routing::get,
    Router,
};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tracing::warn;

use crate::storage::postgres::PostgresDb;

#[derive(Clone)]
pub struct AppState {
    pub db: PostgresDb,
    pub rpc: Arc<RpcClient>,
    pub escrow_authority: Pubkey,
    pub admin_api_token: Option<String>,
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

async fn require_auth(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    if let Some(ref token) = state.admin_api_token {
        if request.uri().path() == "/health" {
            return Ok(next.run(request).await);
        }
        let provided = request
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        match provided {
            Some(t) if constant_time_eq(t.as_bytes(), token.as_bytes()) => {
                Ok(next.run(request).await)
            }
            _ => Err(StatusCode::UNAUTHORIZED),
        }
    } else {
        Ok(next.run(request).await)
    }
}

pub fn router(state: AppState, cors_origin: Option<String>) -> Router {
    let cors = if let Some(ref origin) = cors_origin {
        CorsLayer::new()
            .allow_origin(
                origin
                    .parse::<axum::http::HeaderValue>()
                    .expect("invalid ADMIN_CORS_ORIGIN value"),
            )
            .allow_methods(Any)
            .allow_headers(Any)
    } else {
        if state.admin_api_token.is_some() {
            warn!("ADMIN_CORS_ORIGIN not set — CORS allows any origin");
        }
        CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any)
    };

    Router::new()
        .route("/health", get(routes::health))
        .route("/api/overview", get(routes::overview))
        .route("/api/transactions", get(routes::transactions))
        .route(
            "/api/transactions/{signature}",
            get(routes::transaction_detail),
        )
        .route("/api/pipeline", get(routes::pipeline))
        .route("/api/reconciliation", get(routes::reconciliation))
        .route("/api/checkpoints", get(routes::checkpoints))
        .layer(middleware::from_fn_with_state(state.clone(), require_auth))
        .layer(cors)
        .with_state(state)
}
