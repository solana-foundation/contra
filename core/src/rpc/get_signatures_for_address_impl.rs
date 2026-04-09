use crate::rpc::{
    error::{custom_error, INVALID_PARAMS_CODE, JSON_RPC_SERVER_ERROR},
    ReadDeps,
};
use jsonrpsee::core::RpcResult;
use solana_rpc_client_api::response::RpcConfirmedTransactionStatusWithSignature;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

// Solana RPC spec: limit must be between 1 and 1000 (inclusive).
const DEFAULT_LIMIT: usize = 1000;
const MAX_LIMIT: usize = 1000;

pub async fn get_signatures_for_address_impl(
    read_deps: &ReadDeps,
    address: String,
    config: Option<serde_json::Value>,
) -> RpcResult<Vec<RpcConfirmedTransactionStatusWithSignature>> {
    let pubkey = Pubkey::from_str(&address)
        .map_err(|e| custom_error(INVALID_PARAMS_CODE, format!("Invalid address: {}", e)))?;

    let limit = config
        .as_ref()
        .and_then(|c| c.get("limit"))
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(DEFAULT_LIMIT)
        .clamp(1, MAX_LIMIT);

    let signatures = read_deps
        .accounts_db
        .get_signatures_for_address(&pubkey, limit)
        .await
        .map_err(|e| {
            custom_error(
                JSON_RPC_SERVER_ERROR,
                format!("Failed to get signatures: {}", e),
            )
        })?;

    Ok(signatures)
}
