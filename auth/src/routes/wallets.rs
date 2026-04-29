use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};

use crate::{
    db,
    error::{AppError, AppResult},
    jwt::Claims,
    models::WalletResponse,
    AppState,
};

pub async fn wallets(
    State(state): State<AppState>,
    claims: Claims,
) -> AppResult<Json<Vec<WalletResponse>>> {
    let r = db::list_verified_wallets(&state.pool, claims.sub).await;
    state.pool_status.observe_app(&r);
    let wallets = r?;

    Ok(Json(
        wallets
            .into_iter()
            .map(|w| WalletResponse {
                pubkey: w.pubkey,
                created_at: w.created_at,
            })
            .collect(),
    ))
}

pub async fn delete_wallet(
    State(state): State<AppState>,
    claims: Claims,
    Path(pubkey): Path<String>,
) -> AppResult<StatusCode> {
    let raw = db::delete_verified_wallet(&state.pool, claims.sub, &pubkey).await;
    state.pool_status.observe_sqlx(&raw);
    let deleted = raw.map_err(AppError::Db)?;

    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::BadRequest(
            "wallet not associated with this user".into(),
        ))
    }
}
