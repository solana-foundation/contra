use axum::{extract::State, Json};
use uuid::Uuid;

use crate::{db, error::AppResult, models::ChallengeResponse, AppState, jwt::Claims};         

pub async fn challenge(
    State(state): State<AppState>,
    claims: Claims,
) -> AppResult<Json<ChallengeResponse>> {
    let nonce = Uuid::new_v4();
    let challenge = db::insert_challenge(&state.pool, claims.sub, nonce).await?;

    let message = format!(
        "Contra wallet verification\nuser: {}\nnonce: {}\nexpires: {}",
        claims.sub,
        challenge.nonce,
        challenge.expires_at.timestamp()
    );

    Ok(Json(ChallengeResponse {
        message,
        nonce: challenge.nonce,
        expires_at: challenge.expires_at,
    }))
}