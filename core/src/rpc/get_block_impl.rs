use crate::rpc::{
    error::{custom_error, JSON_RPC_SERVER_ERROR},
    ReadDeps,
};
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
    // A lookup failure is an error, never a "not found": the indexer's slot
    // classifier reads an absent block as a slot it may checkpoint past.
    let block_info =
        read_deps.accounts_db.get_block(slot).await.map_err(|e| {
            custom_error(JSON_RPC_SERVER_ERROR, format!("Failed to get block: {}", e))
        })?;
    let Some(block_info) = block_info else {
        return Ok(None);
    };

    let config = config.map(|c| c.convert_to_current()).unwrap_or_default();

    // Get transactions for this block. A lookup *failure* errors out rather than
    // encoding a block that silently drops a transaction it could not read. A row
    // that is genuinely gone is still skipped: truncation deletes transactions while
    // their block row can survive, so erroring there would break reads of pruned blocks.
    let mut transactions: Vec<TransactionWithStatusMeta> = Vec::new();
    for sig in &block_info.transaction_signatures {
        let stored = read_deps
            .accounts_db
            .get_transaction(sig)
            .await
            .map_err(|e| {
                custom_error(
                    JSON_RPC_SERVER_ERROR,
                    format!("Failed to get block transaction: {}", e),
                )
            })?;
        if let Some(stored_tx) = stored {
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
