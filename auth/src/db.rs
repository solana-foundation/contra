use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    error::AppResult,
    models::{Challenge, Role, User, VerifiedWallet},
};

pub async fn find_user_by_username(
    pool: &PgPool, 
    username: &str
) -> AppResult<Option<User>> {
    let row: Option<(Uuid, String, String, String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT id, username, password_hash, role::text, created_at FROM users WHERE username = $1",
    )
    .bind(username)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|(id, username, password_hash, role, created_at)| User {
        id,
        username,
        password_hash,
        role: match role.as_str() {
            "operator" => Role::Operator,
            _ => Role::User,
        },
        created_at,
    }))
}

pub async fn insert_user(
    pool: &PgPool, 
    username: &str, 
    password_hash: &str
) -> AppResult<User> {
    let row: (Uuid, String, String, String, DateTime<Utc>) = sqlx::query_as(
        "INSERT INTO users (id, username, password_hash, role)
        VALUES ($1, $2, $3, 'user')
        RETURNING id, username, password_hash, role::text, created_at",
    )
    .bind(Uuid::new_v4())
    .bind(username)
    .bind(password_hash)
    .fetch_one(pool)
    .await?;

    Ok(User {
        id: row.0,
        username: row.1,
        password_hash: row.2,
        role: Role::User,
        created_at: row.4,
    })
}
                                                                                        
pub async fn insert_challenge(
    pool: &PgPool,
    user_id: Uuid,
    nonce: Uuid
) -> AppResult<Challenge> {
    let expires_at = Utc::now() + chrono::Duration::minutes(10);

    let row: (Uuid, Uuid, Uuid, DateTime<Utc>, Option<DateTime<Utc>>) = sqlx::query_as(
        "INSERT INTO challenges (id, user_id, nonce, expires_at)
        VALUES ($1, $2, $3, $4)
        RETURNING id, user_id, nonce, expires_at, used_at",
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(nonce)
    .bind(expires_at)
    .fetch_one(pool)
    .await?;

    Ok(Challenge {
        id: row.0,
        user_id: row.1,
        nonce: row.2,
        expires_at: row.3,
        used_at: row.4,
    })
}

pub async fn consume_challenge(
    pool: &PgPool,
    user_id: Uuid,
    nonce: Uuid,
) -> AppResult<Option<Challenge>> {
    let row: Option<(Uuid, Uuid, Uuid, DateTime<Utc>, Option<DateTime<Utc>>)> =
    sqlx::query_as(
        "UPDATE challenges SET used_at = NOW()
        WHERE user_id = $1 AND nonce = $2 AND used_at IS NULL AND expires_at > NOW()
        RETURNING id, user_id, nonce, expires_at, used_at",
    )
    .bind(user_id)
    .bind(nonce)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|(id, user_id, nonce, expires_at, used_at)| Challenge {
        id,
        user_id,
        nonce,
        expires_at,
        used_at,
    }))
}

pub async fn insert_verified_wallet(
    pool: &PgPool,
    user_id: Uuid,
    pubkey: &str,
) -> AppResult<VerifiedWallet> {
    let row: (Uuid, Uuid, String, DateTime<Utc>) = sqlx::query_as(
        "INSERT INTO verified_wallets (id, user_id, pubkey)
        VALUES ($1, $2, $3)
        RETURNING id, user_id, pubkey, created_at",
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(pubkey)
    .fetch_one(pool)
    .await?;

    Ok(VerifiedWallet {
        id: row.0,
        user_id: row.1,
        pubkey: row.2,
        created_at: row.3,
    })
}

pub async fn list_verified_wallets(pool: &PgPool, user_id: Uuid) ->
AppResult<Vec<VerifiedWallet>> {
    let rows: Vec<(Uuid, Uuid, String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT id, user_id, pubkey, created_at FROM verified_wallets WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(id, user_id, pubkey, created_at)| VerifiedWallet {
            id,
            user_id,
            pubkey,
            created_at,
        })
        .collect())
}
