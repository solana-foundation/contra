use crate::rpc::{
    error::{custom_error, INVALID_PARAMS_CODE, JSON_RPC_SERVER_ERROR},
    ReadDeps,
};
use jsonrpsee::core::RpcResult;
use solana_rpc_client_types::config::RpcContextConfig;
use solana_rpc_client_types::response::{Response, RpcResponseContext};
use solana_sdk::hash::Hash;
use std::str::FromStr;

pub async fn is_blockhash_valid_impl(
    read_deps: &ReadDeps,
    blockhash: String,
    _config: Option<RpcContextConfig>,
) -> RpcResult<Response<bool>> {
    // Get the current slot
    let slot = read_deps
        .accounts_db
        .get_latest_slot()
        .await
        .map_err(|e| custom_error(JSON_RPC_SERVER_ERROR, format!("Failed to get slot: {}", e)))?
        .unwrap_or(0);

    // Parse the provided blockhash
    let provided_hash = Hash::from_str(&blockhash)
        .map_err(|e| custom_error(INVALID_PARAMS_CODE, format!("Invalid blockhash: {}", e)))?;

    // Get the latest blockhash
    let latest_hash = read_deps
        .accounts_db
        .get_latest_blockhash()
        .await
        .map_err(|e| {
            custom_error(
                JSON_RPC_SERVER_ERROR,
                format!("Failed to get blockhash: {}", e),
            )
        })?;

    // Check if the blockhash matches the latest one
    // In a production system, you'd want to check a range of recent blockhashes
    // but for now we'll just check the latest one
    let is_valid = provided_hash == latest_hash;

    Ok(Response {
        context: RpcResponseContext::new(slot),
        value: is_valid,
    })
}
