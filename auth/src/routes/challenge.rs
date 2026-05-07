use axum::{extract::State, Json};
use uuid::Uuid;

use crate::{db, error::AppResult, jwt::Claims, models::ChallengeResponse, AppState};

pub async fn challenge(
    State(state): State<AppState>,
    claims: Claims,
) -> AppResult<Json<ChallengeResponse>> {
    let nonce = Uuid::new_v4();
    let r = db::insert_challenge(&state.pool, claims.sub, nonce).await;
    state.pool_status.observe_app(&r);
    let challenge = r?;

    // The message includes user id and nonce so it is bound to this specific user and request.
    // The client must sign this exact string with their wallet's private key.
    let message = format!(
        "PrivateChannel wallet verification\nuser: {}\nnonce: {}\nexpires: {}",
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
