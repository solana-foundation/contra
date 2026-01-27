use crate::rpc::ReadDeps;
use jsonrpsee::core::RpcResult;

pub async fn get_block_time_impl(read_deps: &ReadDeps, slot: u64) -> RpcResult<Option<i64>> {
    Ok(read_deps.accounts_db.get_block_time(slot).await)
}
