pub mod config;
pub mod db;
pub mod error;
pub mod jwt;
pub mod models;
pub mod routes;

use axum::{
    extract::FromRequestParts,
    http::{request::Parts, StatusCode},
    routing::{get, post},
    Json, Router,
};
use sqlx::PgPool;
use std::sync::Arc;
use tower_http::cors::CorsLayer;

use jwt::{Claims, JwtConfig};

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub jwt: Arc<JwtConfig>,
}

// Extract and validate JWT from the Authorization header for any route that declares `claims: Claims`.
impl<S> FromRequestParts<S> for Claims
where
    S: Send + Sync + AsRef<Arc<JwtConfig>>,
{
    type Rejection = (StatusCode, Json<serde_json::Value>);

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let token = parts
            .headers
            .get("Authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(|| {
                (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({ "error": "missing token" })),
                )
            })?;

        state.as_ref().verify(token).map_err(|_| {
            (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({ "error": "invalid token" })),
            )
        })
    }
}

impl AsRef<Arc<JwtConfig>> for AppState {
    fn as_ref(&self) -> &Arc<JwtConfig> {
        &self.jwt
    }
}

pub fn build_app(state: AppState) -> Router {
    Router::new()
        .route("/auth/register", post(routes::register::register))
        .route("/auth/login", post(routes::login::login))
        .route("/auth/challenge-wallet", post(routes::challenge::challenge))
        .route(
            "/auth/verify-wallet",
            post(routes::verify_wallet::verify_wallet),
        )
        .route("/auth/wallets", get(routes::wallets::wallets))
        .route("/health", get(|| async { "ok" }))
        .layer(CorsLayer::permissive())
        .with_state(state)
}
