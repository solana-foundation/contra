use crate::rpc::{
    constants::MAX_SIGNATURES,
    error::{custom_error, INVALID_PARAMS_CODE, JSON_RPC_SERVER_ERROR},
    ReadDeps,
};
use jsonrpsee::core::RpcResult;
use solana_rpc_client_types::config::RpcSignatureStatusConfig;
use solana_rpc_client_types::response::{Response, RpcResponseContext};
use solana_sdk::signature::Signature;
use solana_transaction_status_client_types::{TransactionConfirmationStatus, TransactionStatus};
use std::str::FromStr;
use tracing::{debug, warn};

pub async fn get_signature_statuses_impl(
    read_deps: &ReadDeps,
    signatures: Vec<String>,
    _config: Option<RpcSignatureStatusConfig>,
) -> RpcResult<Response<Vec<Option<TransactionStatus>>>> {
    if signatures.len() > MAX_SIGNATURES {
        return Err(custom_error(
            INVALID_PARAMS_CODE,
            format!(
                "Too many signatures: {} (max: {})",
                signatures.len(),
                MAX_SIGNATURES
            ),
        ));
    }

    let current_slot = read_deps
        .accounts_db
        .get_latest_slot()
        .await
        .map_err(|e| custom_error(JSON_RPC_SERVER_ERROR, format!("Failed to get slot: {}", e)))?
        .unwrap_or(0);

    let mut statuses = Vec::with_capacity(signatures.len());

    for sig_str in signatures {
        // Parse the signature
        let signature = match Signature::from_str(&sig_str) {
            Ok(sig) => sig,
            Err(e) => {
                warn!(
                    signature = %sig_str.get(..20).unwrap_or(&sig_str),
                    error = %e,
                    "Invalid signature format in getSignatureStatuses"
                );
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
                debug!(
                    signature = %signature,
                    status = ?tx.meta.status,
                    err = ?tx.meta.err,
                    "getSignatureStatuses transaction found"
                );

                let err = tx.meta.err.clone();
                statuses.push(Some(TransactionStatus {
                    slot: tx.slot,
                    confirmations: None,
                    status: err.clone().map_or(Ok(()), Err),
                    err,
                    confirmation_status: Some(TransactionConfirmationStatus::Finalized),
                }));
            }
            None => {
                debug!(
                    signature = %signature,
                    "getSignatureStatuses transaction not found"
                );
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
