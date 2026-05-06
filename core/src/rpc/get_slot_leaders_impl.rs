use jsonrpsee::core::RpcResult;

pub async fn get_slot_leaders_impl(_start_slot: u64, _limit: u64) -> RpcResult<Vec<String>> {
    // PrivateChannel has no leader schedule, so we always return an empty array
    Ok(vec![])
}
