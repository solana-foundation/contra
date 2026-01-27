use crate::rpc::{error::custom_error, ReadDeps};
use jsonrpsee::core::RpcResult;
use solana_rpc_client_types::config::RpcContextConfig;

pub async fn get_transaction_count_impl(
    read_deps: &ReadDeps,
    _config: Option<RpcContextConfig>,
) -> RpcResult<u64> {
    read_deps
        .accounts_db
        .get_transaction_count()
        .await
        .map_err(|e| custom_error(-32000, format!("Failed to get transaction count: {}", e)))
}
