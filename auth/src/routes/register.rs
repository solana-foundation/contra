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

const PASSWORD_MIN_LEN: usize = 6;
/// Upper bound matches Argon2's input limit. Argon2 silently truncates passwords
/// longer than 72 bytes, meaning two distinct passwords that share the same first
/// 72 bytes would produce the same hash. Rejecting inputs above the limit surfaces
/// the problem to the caller instead of silently accepting a weaker credential.
const PASSWORD_MAX_LEN: usize = 72;

const USERNAME_MIN_LEN: usize = 5;
const USERNAME_MAX_LEN: usize = 32;

pub async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> AppResult<Json<User>> {
    // Validate username length and characters.
    // Only alphanumeric, underscores, and hyphens are allowed — keeps usernames
    // URL-safe and unambiguous in display contexts.
    if req.username.len() < USERNAME_MIN_LEN {
        return Err(AppError::BadRequest(format!(
            "username must be at least {USERNAME_MIN_LEN} characters"
        )));
    }
    if req.username.len() > USERNAME_MAX_LEN {
        return Err(AppError::BadRequest(format!(
            "username must not exceed {USERNAME_MAX_LEN} characters"
        )));
    }
    if !req
        .username
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        return Err(AppError::BadRequest(
            "username may only contain letters, numbers, underscores, and hyphens".into(),
        ));
    }

    // Validate password length before doing any hashing work.
    if req.password.len() < PASSWORD_MIN_LEN {
        return Err(AppError::BadRequest(format!(
            "password must be at least {PASSWORD_MIN_LEN} characters"
        )));
    }
    if req.password.len() > PASSWORD_MAX_LEN {
        return Err(AppError::BadRequest(format!(
            "password must not exceed {PASSWORD_MAX_LEN} characters"
        )));
    }

    // Hash the password with Argon2 before storing.
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(req.password.as_bytes(), &salt)
        .map_err(|e| AppError::BadRequest(e.to_string()))?
        .to_string();

    // Attempt the INSERT directly rather than doing a SELECT first.
    //
    // The old check-then-insert pattern (find_user_by_username → insert_user) has a TOCTOU
    // race: two concurrent requests for the same username can both pass the existence check,
    // then the second INSERT hits the UNIQUE constraint and returns a 500 instead of 409.
    //
    // The UNIQUE constraint on `username` is atomic at the database level — exactly one
    // concurrent INSERT will succeed. We rely on that guarantee and convert the constraint
    // violation into the correct 409 response here. This is one fewer round-trip and
    // correct under concurrency without any additional transaction isolation.
    //
    // The constraint name `users_username_key` is generated deterministically by Postgres
    // from the table and column name (inline UNIQUE), so it is stable as long as the
    // schema definition in db.rs does not change.
    let user = db::insert_user(&state.pool, &req.username, &hash)
        .await
        .map_err(|e| match e {
            AppError::Db(sqlx::Error::Database(ref db_err))
                if db_err.constraint() == Some("users_username_key") =>
            {
                AppError::Conflict("username already taken".into())
            }
            other => other,
        })?;

    tracing::info!(username = %user.username, "new user registered");

    Ok(Json(user))
}
