use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use thiserror::Error;
use tracing::error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    /// Client is over its per-IP or per-username budget on the credential routes.
    #[error("too many requests")]
    TooManyRequests,
    /// Password hashing is at its concurrency cap. Shedding here keeps the
    /// runtime responsive instead of queueing work without bound.
    #[error("service unavailable")]
    Unavailable,
    /// Wraps unexpected internal failures. The message is logged but never sent to the client.
    #[error("internal error")]
    Internal(#[from] anyhow::Error),
    /// DB errors are caught here and returned as 500 without leaking details.
    #[error("database error")]
    Db(#[from] sqlx::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, self.to_string()),
            AppError::Conflict(msg) => (StatusCode::CONFLICT, msg.clone()),
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            AppError::TooManyRequests => (StatusCode::TOO_MANY_REQUESTS, self.to_string()),
            AppError::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, self.to_string()),
            AppError::Internal(e) => {
                error!("internal error: {e}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_string(),
                )
            }
            AppError::Db(e) => {
                error!("database error: {e}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_string(),
                )
            }
        };

        (status, Json(json!({ "error": message }))).into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;

    fn status(err: AppError) -> StatusCode {
        err.into_response().status()
    }

    #[test]
    fn unauthorized_maps_to_401() {
        assert_eq!(status(AppError::Unauthorized), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn conflict_maps_to_409() {
        assert_eq!(
            status(AppError::Conflict("duplicate username".to_string())),
            StatusCode::CONFLICT,
        );
    }

    #[test]
    fn bad_request_maps_to_400() {
        assert_eq!(
            status(AppError::BadRequest("missing field".to_string())),
            StatusCode::BAD_REQUEST,
        );
    }

    #[test]
    fn internal_maps_to_500() {
        assert_eq!(
            status(AppError::Internal(anyhow::anyhow!("boom"))),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }
}
