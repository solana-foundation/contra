use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::Type, Serialize, Deserialize, PartialEq)]
#[sqlx(type_name = "user_role", rename_all = "lowercase")]
pub enum Role {
    Operator,
    User,
}

// DB rows
pub struct User {
    pub id: Uuid,
    pub username: String,
    pub password_hash: String,
    pub role: Role,
    pub created_at: DateTime<Utc>,
}                                                                                       
                                                                                        
pub struct VerifiedWallet {
    pub id: Uuid,
    pub user_id: Uuid,
    pub pubkey: String,
    pub created_at: DateTime<Utc>,
}
                                                                                        
pub struct Challenge {
    pub id: Uuid,
    pub user_id: Uuid,
    pub nonce: Uuid,
    pub expires_at: DateTime<Utc>,
    pub used_at: Option<DateTime<Utc>>,
}

// Request/response types
#[derive(Deserialize)]
pub struct RegisterRequest {
    pub username: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub token: String,
}

#[derive(Serialize)]
pub struct ChallengeResponse {
    pub message: String,
    pub nonce: Uuid,
    pub expires_at: DateTime<Utc>,
}

#[derive(Deserialize)]
pub struct VerifyWalletRequest {
    pub pubkey: String,
    pub nonce: Uuid,
    pub signature: String,
}

#[derive(Serialize)]
pub struct WalletResponse {
    pub pubkey: String,
    pub created_at: DateTime<Utc>,
}
