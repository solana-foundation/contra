use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::HashMap;

use crate::storage::common::models::{
    DbTransaction, MintDbBalance, TransactionStatus, TransactionType,
};

/// Known mint addresses -> human-readable symbols
pub fn known_mint_symbol(mint: &str) -> String {
    match mint {
        "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v" => "USDC".into(),
        "So11111111111111111111111111111111111111112" => "SOL".into(),
        "mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So" => "mSOL".into(),
        "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB" => "USDT".into(),
        "7dHbWXmci3dT8UFYWYZweBLXgycu7Y3iL6trKn1Y7ARj" => "stSOL".into(),
        "DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263" => "BONK".into(),
        "JUPyiwrYJFskUPiHa7hkeR8VUtAeFoSYbKedZNsDvCN" => "JUP".into(),
        _ => {
            // Truncate unknown mints
            if mint.len() > 8 {
                format!("{}...", &mint[..6])
            } else {
                mint.to_string()
            }
        }
    }
}

fn format_amount(raw: i64, decimals: i16) -> String {
    let divisor = 10f64.powi(decimals as i32);
    let val = raw as f64 / divisor;
    if decimals <= 2 {
        format!("{:.2}", val)
    } else {
        let formatted = format!("{:.2}", val);
        let parts: Vec<&str> = formatted.split('.').collect();
        let int_part = parts[0];
        let dec_part = parts.get(1).unwrap_or(&"00");

        let (sign, digits) = if let Some(d) = int_part.strip_prefix('-') {
            ("-", d)
        } else {
            ("", int_part)
        };
        let with_commas = digits
            .as_bytes()
            .rchunks(3)
            .rev()
            .map(|chunk| std::str::from_utf8(chunk).unwrap())
            .collect::<Vec<_>>()
            .join(",");
        format!("{}{}.{}", sign, with_commas, dec_part)
    }
}

// ── Response types ──────────────────────────────────────────────

#[derive(Serialize)]
pub struct OverviewResponse {
    pub status: &'static str,
    pub programs: Vec<ProgramCheckpoint>,
    pub pipeline_summary: PipelineSummary,
}

#[derive(Serialize)]
pub struct ProgramCheckpoint {
    pub program_type: String,
    pub label: String,
    pub last_indexed_slot: Option<u64>,
    pub chain_head_slot: u64,
    pub slot_lag: u64,
    pub lag_status: &'static str,
}

#[derive(Serialize)]
pub struct PipelineSummary {
    pub pending: i64,
    pub processing: i64,
    pub completed_24h: i64,
    pub failed_24h: i64,
}

#[derive(Serialize)]
pub struct TransactionListResponse {
    pub page: i64,
    pub per_page: i64,
    pub total_count: i64,
    pub transactions: Vec<TransactionResponse>,
}

#[derive(Serialize)]
pub struct TransactionResponse {
    pub id: i64,
    pub signature: String,
    pub slot: i64,
    pub transaction_type: TransactionType,
    pub status: TransactionStatus,
    pub initiator: String,
    pub recipient: String,
    pub mint: String,
    pub mint_symbol: String,
    pub amount: i64,
    pub amount_display: String,
    pub decimals: i16,
    pub memo: Option<String>,
    pub counterpart_signature: Option<String>,
    pub withdrawal_nonce: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub processed_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
    pub latency_ms: Option<i64>,
}

impl TransactionResponse {
    pub fn from_db(tx: DbTransaction, decimals: i16) -> Self {
        let latency_ms = match (tx.processed_at, tx.status) {
            (Some(processed), TransactionStatus::Completed) => {
                Some((processed - tx.created_at).num_milliseconds())
            }
            _ => None,
        };
        let mint_symbol = known_mint_symbol(&tx.mint);
        let amount_display = format_amount(tx.amount, decimals);

        Self {
            id: tx.id,
            signature: tx.signature,
            slot: tx.slot,
            transaction_type: tx.transaction_type,
            status: tx.status,
            initiator: tx.initiator,
            recipient: tx.recipient,
            mint: tx.mint,
            mint_symbol,
            amount: tx.amount,
            amount_display,
            decimals,
            memo: tx.memo,
            counterpart_signature: tx.counterpart_signature,
            withdrawal_nonce: tx.withdrawal_nonce,
            created_at: tx.created_at,
            processed_at: tx.processed_at,
            updated_at: tx.updated_at,
            latency_ms,
        }
    }
}

#[derive(Serialize)]
pub struct PipelineResponse {
    pub by_status: HashMap<String, i64>,
    pub by_type: HashMap<String, i64>,
    pub throughput: ThroughputWindows,
    pub recent_failures: Vec<FailureItem>,
    pub stuck_transactions: Vec<StuckItem>,
}

#[derive(Serialize)]
pub struct ThroughputWindows {
    pub last_1h: ThroughputWindow,
    pub last_24h: ThroughputWindow,
    pub last_7d: ThroughputWindow,
}

#[derive(Serialize)]
pub struct ThroughputWindow {
    pub completed: i64,
    pub failed: i64,
    pub avg_latency_ms: i64,
}

#[derive(Serialize)]
pub struct FailureItem {
    pub id: i64,
    pub signature: String,
    pub transaction_type: TransactionType,
    pub mint_symbol: String,
    pub amount_display: String,
    pub failed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize)]
pub struct StuckItem {
    pub id: i64,
    pub signature: String,
    pub status: TransactionStatus,
    pub transaction_type: TransactionType,
    pub mint_symbol: String,
    pub amount_display: String,
    pub stuck_since: DateTime<Utc>,
    pub age_seconds: i64,
}

#[derive(Serialize)]
pub struct ReconciliationResponse {
    pub checked_at: DateTime<Utc>,
    pub mints: Vec<ReconciliationMint>,
}

#[derive(Serialize)]
pub struct ReconciliationMint {
    pub mint_address: String,
    pub mint_symbol: String,
    pub token_program: String,
    pub decimals: i16,
    pub total_deposits: i64,
    pub total_withdrawals: i64,
    pub indexed_deposits_display: String,
    pub completed_withdrawals_display: String,
    pub expected_balance_display: String,
    pub actual_onchain_balance: Option<i64>,
    pub actual_onchain_balance_display: String,
    pub difference: i64,
    pub difference_display: String,
    pub reconciliation_status: &'static str,
}

impl ReconciliationMint {
    pub fn from_db(balance: MintDbBalance, decimals: i16, onchain_balance: Option<u64>) -> Self {
        let expected = balance.total_deposits - balance.total_withdrawals;
        let actual = onchain_balance.map(|b| b as i64);
        let difference = actual.map(|a| (a - expected).abs()).unwrap_or(0);

        let reconciliation_status = match actual {
            None => "unknown",
            Some(a) if a == expected => "balanced",
            Some(a) => {
                let diff_pct = if expected > 0 {
                    ((a - expected).abs() as f64 / expected as f64) * 100.0
                } else {
                    0.0
                };
                if diff_pct < 0.01 && difference < 1000 {
                    "balanced"
                } else if diff_pct < 1.0 {
                    "warning"
                } else {
                    "mismatch"
                }
            }
        };

        let mint_symbol = known_mint_symbol(&balance.mint_address);

        Self {
            mint_address: balance.mint_address,
            mint_symbol,
            token_program: balance.token_program,
            decimals,
            total_deposits: balance.total_deposits,
            total_withdrawals: balance.total_withdrawals,
            indexed_deposits_display: format_amount(balance.total_deposits, decimals),
            completed_withdrawals_display: format_amount(balance.total_withdrawals, decimals),
            expected_balance_display: format_amount(expected, decimals),
            actual_onchain_balance: actual,
            actual_onchain_balance_display: actual
                .map(|a| format_amount(a, decimals))
                .unwrap_or_else(|| "N/A".into()),
            difference,
            difference_display: format_amount(difference, decimals),
            reconciliation_status,
        }
    }
}

#[derive(Serialize)]
pub struct CheckpointsResponse {
    pub chain_head_slot: u64,
    pub programs: Vec<CheckpointProgram>,
}

#[derive(Serialize)]
pub struct CheckpointProgram {
    pub program_type: String,
    pub label: String,
    pub last_indexed_slot: Option<u64>,
    pub slot_lag: u64,
    pub estimated_time_lag_seconds: u64,
    pub status: &'static str,
}
