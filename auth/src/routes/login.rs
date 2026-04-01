use argon2::{Argon2, PasswordHash, PasswordVerifier};
use axum::{extract::State, Json};

use crate::{
    db,
    error::{AppError, AppResult},
    models::{LoginRequest, LoginResponse},
    AppState,
};

/// A valid Argon2id hash used as a timing sink when the requested username does not exist.
///
/// Without this, an attacker could distinguish "user not found" (fast, no Argon2)
/// from "wrong password" (slow, Argon2 runs) by measuring response time, leaking
/// whether a username is registered.
///
/// Parameters match the argon2 crate defaults: m=19456, t=2, p=1.
/// The plaintext this was derived from is irrelevant and never used for auth.
const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$MTIxYmVlYzFjMGZlZTA4Yjg3MWM3ZmFjYWVjNmE3NzQ$9Z/sQzp5bnPoRwHT/eCB0oYEgk+mm/zhbgBjq8F1CLY";

pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> AppResult<Json<LoginResponse>> {
    let user = db::find_user_by_username(&state.pool, &req.username).await?;

    let Some(user) = user else {
        // User not found — run Argon2 against the dummy hash to match the cost
        // of the "wrong password" path and prevent username enumeration via timing.
        let hash = PasswordHash::new(DUMMY_HASH).expect("dummy hash is valid");
        let _ = Argon2::default().verify_password(req.password.as_bytes(), &hash);
        return Err(AppError::Unauthorized);
    };

    let hash = PasswordHash::new(&user.password_hash).map_err(|_| AppError::Unauthorized)?;
    Argon2::default()
        .verify_password(req.password.as_bytes(), &hash)
        .map_err(|_| AppError::Unauthorized)?;

    let token = state
        .jwt
        .sign(user.id, user.role)
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    Ok(Json(LoginResponse { token }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use argon2::PasswordHash;

    #[test]
    fn dummy_hash_is_valid() {
        assert!(PasswordHash::new(DUMMY_HASH).is_ok());
    }
}
