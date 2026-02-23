use crate::rpc::{
    error::{custom_error, INVALID_PARAMS_CODE},
    ReadDeps,
};
use jsonrpsee::core::RpcResult;
use serde_json::{json, Value};
use solana_rpc_client_types::config::{RpcEncodingConfigWrapper, RpcTransactionConfig};
use solana_sdk::signature::Signature;
use solana_transaction_status_client_types::UiTransactionEncoding;
use std::str::FromStr;

pub async fn get_transaction_impl(
    read_deps: &ReadDeps,
    signature: String,
    config: Option<RpcEncodingConfigWrapper<RpcTransactionConfig>>,
) -> RpcResult<Option<Value>> {
    let sig = Signature::from_str(&signature)
        .map_err(|e| custom_error(INVALID_PARAMS_CODE, format!("Invalid signature: {}", e)))?;

    // Extract encoding from config (default to "json")
    let config = config.map(|c| c.convert_to_current()).unwrap_or_default();

    // Check if the transaction exists using the trait method
    if let Some(stored_tx) = read_deps.accounts_db.get_transaction(&sig).await {
        let encoded_tx = stored_tx
            .encoded_transaction(
                &config.encoding.unwrap_or(UiTransactionEncoding::Json),
                config.max_supported_transaction_version,
            )
            .unwrap();

        Ok(Some(json!(encoded_tx)))
    } else {
        Ok(None)
    }
}
