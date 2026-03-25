pub mod config;
pub mod db;
pub mod error;
pub mod jwt;
pub mod models;
pub mod routes;

use axum::{
    extract::FromRequestParts,
    http::{request::Parts, HeaderValue, Method, StatusCode},
    routing::{get, post},
    Json, Router,
};
use sqlx::PgPool;
use std::sync::Arc;
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};

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

pub fn build_app(state: AppState, cors_allowed_origin: &str) -> Router {
    // Restrict CORS to only what this service actually needs.
    // CorsLayer::permissive() would allow any origin, method, and header — too broad
    // for a service that issues JWTs and handles credentials.
    let origin = if cors_allowed_origin == "*" {
        AllowOrigin::any()
    } else {
        // Parse into a HeaderValue so tower-http can match it exactly.
        // Panic at startup rather than silently falling back to a permissive default.
        let value = HeaderValue::from_str(cors_allowed_origin)
            .expect("CORS_ALLOWED_ORIGIN is not a valid HTTP header value");
        AllowOrigin::exact(value)
    };

    let cors = CorsLayer::new()
        .allow_origin(origin)
        // Only the methods actually used by auth routes.
        .allow_methods(AllowMethods::list([
            Method::GET,
            Method::POST,
            Method::OPTIONS,
        ]))
        // Only the headers clients need to send.
        .allow_headers(AllowHeaders::list([
            axum::http::header::CONTENT_TYPE,
            axum::http::header::AUTHORIZATION,
        ]));

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
        .layer(cors)
        .with_state(state)
}
