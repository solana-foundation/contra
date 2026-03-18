use axum::{extract::State, Json};

use crate::{
    db, error::AppResult, 
    models::WalletResponse, AppState, jwt::Claims
};

pub async fn wallets(
    State(state): State<AppState>,
    claims: Claims,
) -> AppResult<Json<Vec<WalletResponse>>> {
    let wallets = db::list_verified_wallets(&state.pool, claims.sub).await?;

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
