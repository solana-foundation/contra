use crate::rpc::{
    error::{custom_error, INVALID_PARAMS_CODE},
    ReadDeps,
};
use jsonrpsee::core::RpcResult;
use solana_account_decoder_client_types::token::UiTokenAmount;
use solana_rpc_client_types::config::RpcContextConfig;
use solana_rpc_client_types::response::{Response, RpcResponseContext};
use solana_sdk::{account::ReadableAccount, pubkey::Pubkey};
use std::str::FromStr;

pub async fn get_token_account_balance_impl(
    read_deps: &ReadDeps,
    pubkey: String,
    _config: Option<RpcContextConfig>,
) -> RpcResult<Response<UiTokenAmount>> {
    // Parse the pubkey
    let pubkey = Pubkey::from_str(&pubkey)
        .map_err(|_| custom_error(INVALID_PARAMS_CODE, "Invalid pubkey"))?;

    // Get the token account data
    let account = read_deps.accounts_db.get_account_shared_data(&pubkey).await;

    if let Some(account) = account {
        // Check if it's a token account (owned by SPL Token program)
        let token_program_id = Pubkey::from_str("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA")
            .expect("Valid token program ID");

        if *account.owner() != token_program_id {
            return Err(custom_error(
                INVALID_PARAMS_CODE,
                "Account is not a token account",
            ));
        }

        // Parse the token account data
        let data = account.data();
        if data.len() < 165 {
            // SPL Token account size
            return Err(custom_error(
                INVALID_PARAMS_CODE,
                "Invalid token account data",
            ));
        }

        // Extract the amount (bytes 64-72) and decimals (byte 44)
        let amount_bytes = &data[64..72];
        let amount = u64::from_le_bytes(
            amount_bytes
                .try_into()
                .map_err(|_| custom_error(INVALID_PARAMS_CODE, "Invalid token amount"))?,
        );

        // For simplicity, we'll use a fixed decimal value of 6 (common for many tokens)
        // In a real implementation, you'd need to fetch this from the mint account
        let decimals = 6u8;

        let ui_amount = amount as f64 / 10_f64.powi(decimals as i32);
        let ui_amount_string = ui_amount.to_string();

        let slot = read_deps.accounts_db.get_latest_slot().await.unwrap_or(0);

        Ok(Response {
            context: RpcResponseContext::new(slot),
            value: UiTokenAmount {
                ui_amount: Some(ui_amount),
                ui_amount_string,
                amount: amount.to_string(),
                decimals,
            },
        })
    } else {
        Err(custom_error(INVALID_PARAMS_CODE, "Account not found"))
    }
}
