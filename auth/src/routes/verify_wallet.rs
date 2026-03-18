use axum::{extract::State, Json};
use solana_sdk::{pubkey::Pubkey, signature::Signature};
use std::str::FromStr;

use crate::{
    db, 
    error::{AppError, AppResult}, 
    models::{VerifyWalletRequest, WalletResponse}, 
    AppState, 
    jwt::Claims
};

pub async fn verify_wallet(
    State(state): State<AppState>,
    claims: Claims,
    Json(req): Json<VerifyWalletRequest>,
) -> AppResult<Json<WalletResponse>> {
    let challenge = db::consume_challenge(&state.pool, claims.sub, req.nonce)
        .await?
        .ok_or(AppError::BadRequest("invalid or expired challenge".into()))?;

    let message = format!(
        "Contra wallet verification\nuser: {}\nnonce: {}\nexpires: {}",
        claims.sub,
        challenge.nonce,
        challenge.expires_at.timestamp()
    );

    let pubkey = Pubkey::from_str(&req.pubkey)
        .map_err(|_| AppError::BadRequest("invalid pubkey".into()))?;

    let signature = Signature::from_str(&req.signature)
        .map_err(|_| AppError::BadRequest("invalid signature".into()))?;

    if !signature.verify(pubkey.as_ref(), message.as_bytes()) {
        return Err(AppError::Unauthorized);
    }

    let wallet = db::insert_verified_wallet(&state.pool, claims.sub, &req.pubkey).await?;

    Ok(Json(WalletResponse {
        pubkey: wallet.pubkey,
        created_at: wallet.created_at,
    }))
}
