use crate::rpc::{error::custom_error, ReadDeps};
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
    let pubkey = Pubkey::from_str(&pubkey).map_err(|_| custom_error(-32602, "Invalid pubkey"))?;

    // Get the token account data
    let account = read_deps.accounts_db.get_account_shared_data(&pubkey).await;

    if let Some(account) = account {
        // Check if it's a token account (owned by SPL Token program)
        let token_program_id = Pubkey::from_str("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA")
            .expect("Valid token program ID");

        if *account.owner() != token_program_id {
            return Err(custom_error(-32602, "Account is not a token account"));
        }

        // Parse the token account data
        let data = account.data();
        if data.len() < 165 {
            // SPL Token account size
            return Err(custom_error(-32602, "Invalid token account data"));
        }

        // Extract the mint pubkey (bytes 0-32) and amount (bytes 64-72)
        let mint_pubkey = Pubkey::try_from(&data[0..32])
            .map_err(|_| custom_error(-32602, "Invalid mint pubkey in token account"))?;

        let amount_bytes = &data[64..72];
        let amount = u64::from_le_bytes(
            amount_bytes
                .try_into()
                .map_err(|_| custom_error(-32602, "Invalid token amount"))?,
        );

        // Read decimals from the mint account (byte offset 44 in SPL Mint layout)
        let mint_account = read_deps.accounts_db.get_account_shared_data(&mint_pubkey).await;
        let decimals = match mint_account {
            Some(mint) => {
                let mint_data = mint.data();
                if mint_data.len() >= 45 {
                    mint_data[44]
                } else {
                    return Err(custom_error(-32602, "Invalid mint account data"));
                }
            }
            None => {
                return Err(custom_error(-32602, "Mint account not found"));
            }
        };

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
        Err(custom_error(-32602, "Account not found"))
    }
}
