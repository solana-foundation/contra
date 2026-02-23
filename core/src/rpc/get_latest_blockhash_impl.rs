use crate::rpc::{
    error::{custom_error, JSON_RPC_SERVER_ERROR},
    ReadDeps,
};
use jsonrpsee::core::RpcResult;
use solana_rpc_client_types::config::RpcContextConfig;
use solana_rpc_client_types::response::{Response, RpcBlockhash, RpcResponseContext};

pub async fn get_latest_blockhash_impl(
    read_deps: &ReadDeps,
    _config: Option<RpcContextConfig>,
) -> RpcResult<Response<RpcBlockhash>> {
    // Get the latest slot and blockhash from the database
    let slot = read_deps
        .accounts_db
        .get_latest_slot()
        .await
        .map_err(|e| custom_error(JSON_RPC_SERVER_ERROR, format!("Failed to get slot: {}", e)))?;
    let blockhash = read_deps
        .accounts_db
        .get_latest_blockhash()
        .await
        .map_err(|e| custom_error(JSON_RPC_SERVER_ERROR, format!("Failed to get blockhash: {}", e)))?;

    // Calculate last valid block height
    // In Solana, a blockhash is valid for approximately 150 blocks
    // We'll use slot as the block height and add 150 for validity period
    // TODO: We will have different expiration logic, which means it may not
    // necessarily be 150 blocks
    let last_valid_block_height = slot + 150;

    Ok(Response {
        context: RpcResponseContext::new(slot),
        value: RpcBlockhash {
            blockhash: blockhash.to_string(),
            last_valid_block_height,
        },
    })
}
