use crate::rpc::{error::custom_error, ReadDeps};
use jsonrpsee::core::RpcResult;
use solana_rpc_client_types::config::RpcContextConfig;

pub async fn get_slot_impl(
    read_deps: &ReadDeps,
    _config: Option<RpcContextConfig>,
) -> RpcResult<u64> {
    read_deps
        .accounts_db
        .get_latest_slot()
        .await
        .map_err(|e| custom_error(-32000, format!("Failed to get slot: {}", e)))
}
