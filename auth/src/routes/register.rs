use argon2::{
    password_hash::{rand_core::OsRng, PasswordHasher, SaltString},
    Argon2,
};
use axum::{extract::State, Json};

use crate::{
    db,
    error::{AppError, AppResult},
    models::{RegisterRequest, User},
    AppState,
};

pub async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> AppResult<Json<User>> {
    if db::find_user_by_username(&state.pool, &req.username)
        .await?
        .is_some()
    {
        return Err(AppError::Conflict("username already taken".into()));
    }

    // Hash the password with Argon2 before storing.
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(req.password.as_bytes(), &salt)
        .map_err(|e| AppError::BadRequest(e.to_string()))?
        .to_string();

    let user = db::insert_user(&state.pool, &req.username, &hash).await?;

    tracing::info!(username = %user.username, "new user registered");

    Ok(Json(user))
}
