use crate::rpc::{
    error::{custom_error, JSON_RPC_SERVER_ERROR},
    ReadDeps,
};
use jsonrpsee::core::RpcResult;
use solana_rpc_client_types::config::RpcSignatureStatusConfig;
use solana_rpc_client_types::response::{Response, RpcResponseContext};
use solana_sdk::signature::Signature;
use solana_transaction_error::TransactionError;
use solana_transaction_status_client_types::{TransactionConfirmationStatus, TransactionStatus};
use std::str::FromStr;
use tracing::info;

pub async fn get_signature_statuses_impl(
    read_deps: &ReadDeps,
    signatures: Vec<String>,
    _config: Option<RpcSignatureStatusConfig>,
) -> RpcResult<Response<Vec<Option<TransactionStatus>>>> {
    let current_slot =
        read_deps.accounts_db.get_latest_slot().await.map_err(|e| {
            custom_error(JSON_RPC_SERVER_ERROR, format!("Failed to get slot: {}", e))
        })?;

    let mut statuses = Vec::with_capacity(signatures.len());

    for sig_str in signatures {
        // Parse the signature
        let signature = match Signature::from_str(&sig_str) {
            Ok(sig) => sig,
            Err(_) => {
                // Invalid signature format - return null for this entry
                statuses.push(None);
                continue;
            }
        };

        // Check if transaction exists
        let stored_tx = read_deps.accounts_db.get_transaction(&signature).await;

        match stored_tx {
            Some(tx) => {
                // Transaction found - return its status
                // In Contra, all found transactions are confirmed (finalized)
                info!(
                    "Transaction found: {} {:?} err: {:?}",
                    signature, tx.meta.status, tx.meta.err
                );

                let err = tx.meta.err.clone();
                let status: Result<(), TransactionError> = match err {
                    None => Ok(()),
                    Some(ref e) => Err(e.clone()),
                };

                statuses.push(Some(TransactionStatus {
                    slot: tx.slot,
                    confirmations: None, // null means "rooted" (finalized)
                    status,
                    err,
                    confirmation_status: Some(TransactionConfirmationStatus::Finalized),
                }));
            }
            None => {
                info!("Transaction not found: {}", signature);
                // Transaction not found
                statuses.push(None);
            }
        }
    }

    Ok(Response {
        context: RpcResponseContext::new(current_slot),
        value: statuses,
    })
}
