use crate::rpc::ReadDeps;
use jsonrpsee::core::RpcResult;
use serde_json::{json, Value};
use solana_rpc_client_types::config::{RpcBlockConfig, RpcEncodingConfigWrapper};
use solana_transaction_status::{BlockEncodingOptions, ConfirmedBlock, TransactionWithStatusMeta};
use solana_transaction_status_client_types::{TransactionDetails, UiTransactionEncoding};

pub async fn get_block_impl(
    read_deps: &ReadDeps,
    slot: u64,
    config: Option<RpcEncodingConfigWrapper<RpcBlockConfig>>,
) -> RpcResult<Option<Value>> {
    // Get block data using the trait method
    let block_info = match read_deps.accounts_db.get_block(slot).await {
        Some(block) => block,
        None => return Ok(None),
    };

    let config = config.map(|c| c.convert_to_current()).unwrap_or_default();

    // Get transactions for this block
    let mut transactions: Vec<TransactionWithStatusMeta> = Vec::new();
    for sig in &block_info.transaction_signatures {
        if let Some(stored_tx) = read_deps.accounts_db.get_transaction(sig).await {
            transactions.push(stored_tx.transaction_with_status_meta());
        }
    }

    let confirmed_block = ConfirmedBlock {
        block_time: block_info.block_time,
        block_height: block_info.block_height,
        previous_blockhash: block_info.previous_blockhash.to_string(),
        blockhash: block_info.blockhash.to_string(),
        parent_slot: block_info.parent_slot,
        transactions,
        rewards: vec![],
        num_partitions: None,
    };
    let encoded_block = confirmed_block
        .encode_with_options(
            config.encoding.unwrap_or(UiTransactionEncoding::Json),
            BlockEncodingOptions {
                transaction_details: config
                    .transaction_details
                    .unwrap_or(TransactionDetails::Full),
                show_rewards: config.rewards.unwrap_or(true),
                max_supported_transaction_version: config.max_supported_transaction_version,
            },
        )
        .unwrap();

    Ok(Some(json!(encoded_block)))
}
