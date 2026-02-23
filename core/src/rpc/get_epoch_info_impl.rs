use crate::rpc::{
    error::{custom_error, JSON_RPC_SERVER_ERROR},
    ReadDeps,
};
use jsonrpsee::core::RpcResult;
use solana_epoch_info::EpochInfo;
use solana_rpc_client_types::config::RpcEpochConfig;

pub async fn get_epoch_info_impl(
    read_deps: &ReadDeps,
    _config: Option<RpcEpochConfig>,
) -> RpcResult<EpochInfo> {
    read_deps
        .accounts_db
        .get_epoch_info()
        .await
        .map_err(|e| custom_error(JSON_RPC_SERVER_ERROR, format!("Failed to get epoch info: {}", e)))
}
