use crate::rpc::{error::custom_error, ReadDeps};
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
        .map_err(|e| custom_error(-32000, format!("Failed to get slot: {}", e)))?;

    // Parse the provided blockhash
    let provided_hash = Hash::from_str(&blockhash)
        .map_err(|e| custom_error(-32602, format!("Invalid blockhash: {}", e)))?;

    // Check if the blockhash is in the live blockhash window
    // This validates against the full window maintained by the Dedup stage,
    // not just the single latest blockhash, upholding security invariant C4
    //
    // Edge cases handled:
    // - Empty window: iter().any() returns false (all blockhashes rejected at startup)
    // - Lock poisoning: Properly handled with map_err instead of unwrap()
    let live_blockhashes = read_deps
        .live_blockhashes
        .read()
        .map_err(|e| custom_error(-32603, format!("Failed to acquire blockhash lock: {}", e)))?;

    let is_valid = live_blockhashes.iter().any(|h| h == &provided_hash);

    Ok(Response {
        context: RpcResponseContext::new(slot),
        value: is_valid,
    })
}
