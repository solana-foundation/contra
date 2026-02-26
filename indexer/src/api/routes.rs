use std::collections::HashMap;
use std::time::Duration;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use serde::Deserialize;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use spl_associated_token_account::get_associated_token_address_with_program_id;
use tracing::{error, warn};

use super::types::*;
use super::AppState;
use crate::storage::common::models::{TransactionStatus, TransactionType};

// ── Health ──────────────────────────────────────────────────────

pub async fn health(State(state): State<AppState>) -> impl IntoResponse {
    match sqlx::query("SELECT 1").execute(state.db.pool()).await {
        Ok(_) => (StatusCode::OK, "ok"),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "db unreachable"),
    }
}

// ── Overview ────────────────────────────────────────────────────

pub async fn overview(State(state): State<AppState>) -> Result<Json<OverviewResponse>, StatusCode> {
    let chain_head = state
        .rpc
        .get_slot()
        .await
        .map_err(log_err(StatusCode::BAD_GATEWAY))?;

    let escrow_slot = state
        .db
        .get_committed_checkpoint_internal("escrow")
        .await
        .map_err(log_err(StatusCode::INTERNAL_SERVER_ERROR))?;

    let withdraw_slot = state
        .db
        .get_committed_checkpoint_internal("withdraw")
        .await
        .map_err(log_err(StatusCode::INTERNAL_SERVER_ERROR))?;

    let status_counts_24h = state
        .db
        .get_24h_status_counts()
        .await
        .map_err(log_err(StatusCode::INTERNAL_SERVER_ERROR))?;

    let status_counts = state
        .db
        .get_status_counts()
        .await
        .map_err(log_err(StatusCode::INTERNAL_SERVER_ERROR))?;

    let mut pending: i64 = 0;
    let mut processing: i64 = 0;
    for (s, c) in &status_counts {
        match s {
            TransactionStatus::Pending => pending = *c,
            TransactionStatus::Processing => processing = *c,
            _ => {}
        }
    }

    let mut completed_24h: i64 = 0;
    let mut failed_24h: i64 = 0;
    for (s, c) in &status_counts_24h {
        match s {
            TransactionStatus::Completed => completed_24h = *c,
            TransactionStatus::Failed => failed_24h = *c,
            _ => {}
        }
    }

    fn make_program(
        program_type: &str,
        label: &str,
        slot: Option<u64>,
        chain_head: u64,
    ) -> ProgramCheckpoint {
        let lag = slot.map(|s| chain_head.saturating_sub(s)).unwrap_or(0);
        let lag_status = if lag < 100 {
            "ok"
        } else if lag < 500 {
            "warning"
        } else {
            "critical"
        };
        ProgramCheckpoint {
            program_type: program_type.into(),
            label: label.into(),
            last_indexed_slot: slot,
            chain_head_slot: chain_head,
            slot_lag: lag,
            lag_status,
        }
    }

    let has_errors = failed_24h > 0 || pending > 10;
    let system_status = if has_errors { "degraded" } else { "healthy" };

    Ok(Json(OverviewResponse {
        status: system_status,
        programs: vec![
            make_program("escrow", "Deposits (Escrow)", escrow_slot, chain_head),
            make_program("withdraw", "Withdrawals", withdraw_slot, chain_head),
        ],
        pipeline_summary: PipelineSummary {
            pending,
            processing,
            completed_24h,
            failed_24h,
        },
    }))
}

// ── Transactions ────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct TransactionListParams {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    pub status: Option<String>,
    #[serde(rename = "type")]
    pub tx_type: Option<String>,
}

pub async fn transactions(
    State(state): State<AppState>,
    Query(params): Query<TransactionListParams>,
) -> Result<Json<TransactionListResponse>, StatusCode> {
    let page = params.page.unwrap_or(1).max(1);
    let per_page = params.per_page.unwrap_or(25).clamp(1, 100);

    let status = params.status.and_then(|s| match s.to_lowercase().as_str() {
        "pending" => Some(TransactionStatus::Pending),
        "processing" => Some(TransactionStatus::Processing),
        "completed" => Some(TransactionStatus::Completed),
        "failed" => Some(TransactionStatus::Failed),
        _ => None,
    });

    let tx_type = params
        .tx_type
        .and_then(|t| match t.to_lowercase().as_str() {
            "deposit" => Some(TransactionType::Deposit),
            "withdrawal" => Some(TransactionType::Withdrawal),
            _ => None,
        });

    let (rows, total) = state
        .db
        .get_transactions_paginated(page, per_page, status, tx_type)
        .await
        .map_err(log_err(StatusCode::INTERNAL_SERVER_ERROR))?;

    let decimals_map = get_all_decimals(&state).await?;

    let transactions: Vec<TransactionResponse> = rows
        .into_iter()
        .map(|tx| {
            let decimals = decimals_map.get(&tx.mint).copied().unwrap_or(0);
            TransactionResponse::from_db(tx, decimals)
        })
        .collect();

    Ok(Json(TransactionListResponse {
        page,
        per_page,
        total_count: total,
        transactions,
    }))
}

pub async fn transaction_detail(
    State(state): State<AppState>,
    Path(signature): Path<String>,
) -> Result<Json<TransactionResponse>, StatusCode> {
    let tx = state
        .db
        .get_transaction_by_signature(&signature)
        .await
        .map_err(log_err(StatusCode::INTERNAL_SERVER_ERROR))?
        .ok_or(StatusCode::NOT_FOUND)?;

    let decimals = state
        .db
        .get_mint_internal(&tx.mint)
        .await
        .map_err(log_err(StatusCode::INTERNAL_SERVER_ERROR))?
        .map(|m| m.decimals)
        .unwrap_or(0);

    Ok(Json(TransactionResponse::from_db(tx, decimals)))
}

// ── Pipeline ────────────────────────────────────────────────────

pub async fn pipeline(State(state): State<AppState>) -> Result<Json<PipelineResponse>, StatusCode> {
    let status_counts = state
        .db
        .get_status_counts()
        .await
        .map_err(log_err(StatusCode::INTERNAL_SERVER_ERROR))?;

    let type_counts = state
        .db
        .get_type_counts()
        .await
        .map_err(log_err(StatusCode::INTERNAL_SERVER_ERROR))?;

    let tp_1h = state
        .db
        .get_throughput_window(3600)
        .await
        .map_err(log_err(StatusCode::INTERNAL_SERVER_ERROR))?;

    let tp_24h = state
        .db
        .get_throughput_window(86400)
        .await
        .map_err(log_err(StatusCode::INTERNAL_SERVER_ERROR))?;

    let tp_7d = state
        .db
        .get_throughput_window(604800)
        .await
        .map_err(log_err(StatusCode::INTERNAL_SERVER_ERROR))?;

    let failures = state
        .db
        .get_recent_failures(10)
        .await
        .map_err(log_err(StatusCode::INTERNAL_SERVER_ERROR))?;

    let stuck = state
        .db
        .get_stuck_transactions(300, 10)
        .await
        .map_err(log_err(StatusCode::INTERNAL_SERVER_ERROR))?;

    let all_mints = get_all_decimals(&state).await?;
    let now = Utc::now();

    let by_status: HashMap<String, i64> = status_counts
        .into_iter()
        .map(|(s, c)| (format!("{:?}", s), c))
        .collect();

    let by_type: HashMap<String, i64> = type_counts
        .into_iter()
        .map(|(t, c)| (format!("{:?}", t), c))
        .collect();

    let failure_items: Vec<FailureItem> = failures
        .into_iter()
        .map(|tx| {
            let decimals = all_mints.get(&tx.mint).copied().unwrap_or(0);
            FailureItem {
                id: tx.id,
                signature: tx.signature,
                transaction_type: tx.transaction_type,
                mint_symbol: known_mint_symbol(&tx.mint),
                amount_display: format_amount_simple(tx.amount, decimals),
                failed_at: tx.updated_at,
                created_at: tx.created_at,
            }
        })
        .collect();

    let stuck_items: Vec<StuckItem> = stuck
        .into_iter()
        .map(|tx| {
            let decimals = all_mints.get(&tx.mint).copied().unwrap_or(0);
            let age = (now - tx.updated_at).num_seconds();
            StuckItem {
                id: tx.id,
                signature: tx.signature,
                status: tx.status,
                transaction_type: tx.transaction_type,
                mint_symbol: known_mint_symbol(&tx.mint),
                amount_display: format_amount_simple(tx.amount, decimals),
                stuck_since: tx.updated_at,
                age_seconds: age,
            }
        })
        .collect();

    fn make_window((completed, failed, avg): (i64, i64, Option<f64>)) -> ThroughputWindow {
        ThroughputWindow {
            completed,
            failed,
            avg_latency_ms: avg.unwrap_or(0.0) as i64,
        }
    }

    Ok(Json(PipelineResponse {
        by_status,
        by_type,
        throughput: ThroughputWindows {
            last_1h: make_window(tp_1h),
            last_24h: make_window(tp_24h),
            last_7d: make_window(tp_7d),
        },
        recent_failures: failure_items,
        stuck_transactions: stuck_items,
    }))
}

// ── Reconciliation ──────────────────────────────────────────────

pub async fn reconciliation(
    State(state): State<AppState>,
) -> Result<Json<ReconciliationResponse>, StatusCode> {
    let balances = state
        .db
        .get_mint_balances_for_reconciliation_internal()
        .await
        .map_err(log_err(StatusCode::INTERNAL_SERVER_ERROR))?;

    let all_mints = get_all_decimals(&state).await?;

    // Fetch all ATA balances concurrently with per-call timeout
    let rpc_futures: Vec<_> = balances
        .iter()
        .map(|balance| {
            let rpc = state.rpc.clone();
            let mint_address = balance.mint_address.clone();
            let token_program = balance.token_program.clone();
            let escrow_authority = state.escrow_authority;
            async move {
                tokio::time::timeout(
                    Duration::from_secs(10),
                    fetch_ata_balance(&rpc, &mint_address, &token_program, &escrow_authority),
                )
                .await
                .unwrap_or(None)
            }
        })
        .collect();

    let onchain_balances = futures::future::join_all(rpc_futures).await;

    let mints: Vec<ReconciliationMint> = balances
        .into_iter()
        .zip(onchain_balances)
        .map(|(balance, onchain)| {
            let decimals = all_mints.get(&balance.mint_address).copied().unwrap_or(0);
            ReconciliationMint::from_db(balance, decimals, onchain)
        })
        .collect();

    Ok(Json(ReconciliationResponse {
        checked_at: Utc::now(),
        mints,
    }))
}

// ── Checkpoints ─────────────────────────────────────────────────

pub async fn checkpoints(
    State(state): State<AppState>,
) -> Result<Json<CheckpointsResponse>, StatusCode> {
    let chain_head = state
        .rpc
        .get_slot()
        .await
        .map_err(log_err(StatusCode::BAD_GATEWAY))?;

    let escrow_slot = state
        .db
        .get_committed_checkpoint_internal("escrow")
        .await
        .map_err(log_err(StatusCode::INTERNAL_SERVER_ERROR))?;

    let withdraw_slot = state
        .db
        .get_committed_checkpoint_internal("withdraw")
        .await
        .map_err(log_err(StatusCode::INTERNAL_SERVER_ERROR))?;

    fn make_program(
        program_type: &str,
        label: &str,
        slot: Option<u64>,
        chain_head: u64,
    ) -> CheckpointProgram {
        let lag = slot.map(|s| chain_head.saturating_sub(s)).unwrap_or(0);
        // Rough estimate: ~0.4s per slot
        let time_lag = (lag as f64 * 0.4) as u64;
        let status = if lag < 100 {
            "ok"
        } else if lag < 500 {
            "warning"
        } else {
            "critical"
        };
        CheckpointProgram {
            program_type: program_type.into(),
            label: label.into(),
            last_indexed_slot: slot,
            slot_lag: lag,
            estimated_time_lag_seconds: time_lag,
            status,
        }
    }

    Ok(Json(CheckpointsResponse {
        chain_head_slot: chain_head,
        programs: vec![
            make_program(
                "escrow",
                "Deposits (Escrow Indexer)",
                escrow_slot,
                chain_head,
            ),
            make_program(
                "withdraw",
                "Withdrawals (Withdraw Indexer)",
                withdraw_slot,
                chain_head,
            ),
        ],
    }))
}

// ── Helpers ─────────────────────────────────────────────────────

fn log_err<E: std::fmt::Display>(status: StatusCode) -> impl Fn(E) -> StatusCode {
    move |e| {
        error!("{status}: {e}");
        status
    }
}

async fn get_all_decimals(state: &AppState) -> Result<HashMap<String, i16>, StatusCode> {
    Ok(state
        .db
        .get_all_mints()
        .await
        .map_err(log_err(StatusCode::INTERNAL_SERVER_ERROR))?
        .into_iter()
        .map(|m| (m.mint_address, m.decimals))
        .collect())
}

async fn fetch_ata_balance(
    rpc: &RpcClient,
    mint_address: &str,
    token_program: &str,
    escrow_authority: &Pubkey,
) -> Option<u64> {
    let mint_pk: Pubkey = mint_address.parse().ok()?;
    let token_program_pk: Pubkey = token_program.parse().ok()?;
    let ata =
        get_associated_token_address_with_program_id(escrow_authority, &mint_pk, &token_program_pk);

    match rpc.get_token_account_balance(&ata).await {
        Ok(balance) => balance.amount.parse::<u64>().ok(),
        Err(e) => {
            warn!(
                "Failed to fetch ATA balance for mint {}: {}",
                mint_address, e
            );
            None
        }
    }
}

fn format_amount_simple(raw: i64, decimals: i16) -> String {
    let divisor = 10f64.powi(decimals as i32);
    let val = raw as f64 / divisor;
    format!("{:.2}", val)
}
