use crate::rpc::{error::custom_error, ReadDeps};
use jsonrpsee::core::RpcResult;
use solana_rpc_client_types::response::RpcPerfSample;

pub async fn get_recent_performance_samples_impl(
    read_deps: &ReadDeps,
    limit: Option<usize>,
) -> RpcResult<Vec<RpcPerfSample>> {
    // Default to 720 and enforce maximum limit as per Solana spec
    const MAX_SAMPLES: usize = 720;
    let limit = limit.unwrap_or(MAX_SAMPLES).min(MAX_SAMPLES);

    read_deps
        .accounts_db
        .get_recent_performance_samples(limit)
        .await
        .map_err(|e| {
            custom_error(
                -32000,
                format!("Failed to get recent performance samples: {}", e),
            )
        })
}
