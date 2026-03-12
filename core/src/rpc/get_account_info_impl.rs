use crate::{
    accounts::bob::BOB,
    rpc::{
        error::{custom_error, INVALID_PARAMS_CODE, JSON_RPC_SERVER_ERROR},
        ReadDeps,
    },
};
use jsonrpsee::core::RpcResult;
use solana_account_decoder::{
    encode_ui_account,
    parse_account_data::{AccountAdditionalDataV3, SplTokenAdditionalDataV2},
};
use solana_account_decoder_client_types::{UiAccount, UiAccountEncoding};
use solana_client::{
    rpc_config::RpcAccountInfoConfig,
    rpc_response::{Response, RpcResponseContext},
};
use solana_sdk::{account::ReadableAccount, pubkey::Pubkey};
use solana_svm_callback::TransactionProcessingCallback;
use spl_token::solana_program::program_pack::Pack;
use spl_token::state::{Account as TokenAccount, Mint};
use std::str::FromStr;
use tokio::sync::mpsc;
use tracing::info;

pub async fn get_account_info_impl(
    read_deps: &ReadDeps,
    pubkey: String,
    config: Option<RpcAccountInfoConfig>,
) -> RpcResult<Response<Option<UiAccount>>> {
    let pubkey = Pubkey::from_str(&pubkey)
        .map_err(|e| custom_error(INVALID_PARAMS_CODE, format!("Invalid pubkey: {}", e)))?;

    let config = config.unwrap_or_default();

    let slot = read_deps
        .accounts_db
        .get_latest_slot()
        .await
        .map_err(|e| custom_error(JSON_RPC_SERVER_ERROR, format!("Failed to get slot: {}", e)))?
        .unwrap_or(0);

    // Get account from database
    let (_settled_accounts_tx, settled_accounts_rx) = mpsc::unbounded_channel();
    let bob = BOB::new(read_deps.accounts_db.clone(), settled_accounts_rx).await;
    let account_data = bob.get_account_shared_data(&pubkey);
    let encoding = config.encoding.unwrap_or(UiAccountEncoding::Base64);

    let additional_data = if encoding == UiAccountEncoding::JsonParsed {
        build_token_additional_data(account_data.as_ref(), &bob)
    } else {
        None
    };

    let value = account_data.map(|account| {
        encode_ui_account(&pubkey, &account, encoding, additional_data, config.data_slice)
    });
    info!("Account info: {:?}", value);

    // TODO: Get actual slot from the read node's state
    Ok(Response {
        context: RpcResponseContext::new(slot),
        value,
    })
}

fn build_token_additional_data(
    account: Option<&solana_sdk::account::AccountSharedData>,
    bob: &BOB,
) -> Option<AccountAdditionalDataV3> {
    let account = account?;
    if *account.owner() != spl_token::id() {
        return None;
    }
    let token_account = TokenAccount::unpack(account.data()).ok()?;
    let mint_account = bob.get_account_shared_data(&token_account.mint)?;
    let mint = Mint::unpack(mint_account.data()).ok()?;
    Some(AccountAdditionalDataV3 {
        spl_token_additional_data: Some(SplTokenAdditionalDataV2 {
            decimals: mint.decimals,
            ..Default::default()
        }),
    })
}
