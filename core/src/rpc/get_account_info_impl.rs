use crate::{
    accounts::bob::BOB,
    rpc::{error::custom_error, ReadDeps},
};
use jsonrpsee::core::RpcResult;
use solana_account_decoder::encode_ui_account;
use solana_account_decoder_client_types::{UiAccount, UiAccountEncoding};
use solana_client::{
    rpc_config::RpcAccountInfoConfig,
    rpc_response::{Response, RpcResponseContext},
};
use solana_sdk::pubkey::Pubkey;
use solana_svm_callback::TransactionProcessingCallback;
use std::str::FromStr;
use tokio::sync::mpsc;
use tracing::info;

pub async fn get_account_info_impl(
    read_deps: &ReadDeps,
    pubkey: String,
    config: Option<RpcAccountInfoConfig>,
) -> RpcResult<Response<Option<UiAccount>>> {
    let pubkey = Pubkey::from_str(&pubkey)
        .map_err(|e| custom_error(-32602, format!("Invalid pubkey: {}", e)))?;

    let config = config.unwrap_or_default();

    let slot = read_deps
        .accounts_db
        .get_latest_slot()
        .await
        .map_err(|e| custom_error(-32000, format!("Failed to get slot: {}", e)))?;

    // Get account from database
    let (_settled_accounts_tx, settled_accounts_rx) = mpsc::unbounded_channel();
    let bob = BOB::new(read_deps.accounts_db.clone(), settled_accounts_rx).await;
    let account_data = bob.get_account_shared_data(&pubkey);
    let encoding = config.encoding.unwrap_or(UiAccountEncoding::Base64);
    let value = account_data.map(|account| {
        // Encode data based on requested encoding
        encode_ui_account(&pubkey, &account, encoding, None, config.data_slice)
    });
    info!("Account info: {:?}", value);

    // TODO: Get actual slot from the read node's state
    Ok(Response {
        context: RpcResponseContext::new(slot),
        value,
    })
}
