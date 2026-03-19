use argon2::{Argon2, PasswordHash, PasswordVerifier};
use axum::{extract::State, Json};

use crate::{
    db,
    error::{AppError, AppResult},
    models::{LoginRequest, LoginResponse},
    AppState,
};

pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> AppResult<Json<LoginResponse>> {
    // Return 401 for both "user not found" and "wrong password" to avoid username enumeration.
    let user = db::find_user_by_username(&state.pool, &req.username)
        .await?
        .ok_or(AppError::Unauthorized)?;

    let hash = PasswordHash::new(&user.password_hash)
        .map_err(|_| AppError::Unauthorized)?;

    Argon2::default()
        .verify_password(req.password.as_bytes(), &hash)
        .map_err(|_| AppError::Unauthorized)?;

    let token = state.jwt.sign(user.id, user.role)
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    Ok(Json(LoginResponse { token }))
}
