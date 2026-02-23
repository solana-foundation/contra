use crate::rpc::{
    error::{custom_error, JSON_RPC_SERVER_ERROR},
    ReadDeps,
};
use jsonrpsee::core::RpcResult;

pub async fn get_first_available_block_impl(read_deps: &ReadDeps) -> RpcResult<u64> {
    read_deps
        .accounts_db
        .get_first_available_block()
        .await
        .map_err(|e| {
            custom_error(
                JSON_RPC_SERVER_ERROR,
                format!("Failed to get first available block: {}", e),
            )
        })
}
