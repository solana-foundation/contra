use jsonrpsee::core::RpcResult;
use solana_rpc_client_types::{config::RpcGetVoteAccountsConfig, response::RpcVoteAccountStatus};

pub async fn get_vote_accounts_impl(
    _config: Option<RpcGetVoteAccountsConfig>,
) -> RpcResult<RpcVoteAccountStatus> {
    // Contra has no voting/staking mechanism, so both arrays are empty
    Ok(RpcVoteAccountStatus {
        current: vec![],
        delinquent: vec![],
    })
}
