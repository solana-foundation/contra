pub mod config;
pub mod db;
pub mod error;
pub mod jwt;
pub mod models;
pub mod password;
pub mod pool_status;
pub mod routes;
pub mod throttle;
pub mod validation;

use axum::{
    extract::{DefaultBodyLimit, FromRequestParts, State},
    http::{request::Parts, HeaderValue, Method, StatusCode},
    middleware,
    routing::{delete, get, post},
    Json, Router,
};
use sqlx::PgPool;
use std::sync::Arc;
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};

use jwt::{Claims, JwtConfig};
use password::PasswordWorker;
use pool_status::PoolStatus;
use throttle::AuthThrottle;

/// Body cap for the credential routes, sized to the username and password
/// limits plus JSON overhead. The rest of the router keeps axum's default.
const CREDENTIAL_BODY_LIMIT: usize = 4096;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub jwt: Arc<JwtConfig>,
    pub pool_status: Arc<PoolStatus>,
    pub passwords: PasswordWorker,
    pub throttle: Arc<AuthThrottle>,
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
            Method::DELETE,
            Method::OPTIONS,
        ]))
        // Only the headers clients need to send.
        .allow_headers(AllowHeaders::list([
            axum::http::header::CONTENT_TYPE,
            axum::http::header::AUTHORIZATION,
        ]));

    // The credential routes are the expensive unauthenticated surface: each one
    // runs Argon2. They get the throttle and a tight body limit; the throttle
    // is layered last so it runs before the body is read.
    let credentials = Router::new()
        .route("/auth/register", post(routes::register::register))
        .route("/auth/login", post(routes::login::login))
        .layer(DefaultBodyLimit::max(CREDENTIAL_BODY_LIMIT))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            throttle::per_ip,
        ));

    Router::new()
        .merge(credentials)
        .route("/auth/challenge-wallet", post(routes::challenge::challenge))
        .route(
            "/auth/verify-wallet",
            post(routes::verify_wallet::verify_wallet),
        )
        .route("/auth/wallets", get(routes::wallets::wallets))
        .route(
            "/auth/wallets/{pubkey}",
            delete(routes::wallets::delete_wallet),
        )
        .route("/health", get(health))
        .layer(cors)
        .with_state(state)
}

/// Reads the cached pool-status flag updated by handlers; doesn't itself touch the pool.
async fn health(State(state): State<AppState>) -> StatusCode {
    if state.pool_status.is_healthy() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}
