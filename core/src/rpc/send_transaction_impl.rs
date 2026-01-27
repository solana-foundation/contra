use crate::rpc::{error::custom_error, WriteDeps};
use base64::{engine::general_purpose::STANDARD, Engine};
use bincode::Options;
use jsonrpsee::core::RpcResult;
use solana_rpc_client_types::config::RpcSendTransactionConfig;
use solana_runtime_transaction::runtime_transaction::RuntimeTransaction;
use solana_sdk::{
    message::{v0::LoadedAddresses, SimpleAddressLoader},
    transaction::{MessageHash, VersionedTransaction},
};
use std::collections::HashSet;
use tracing::{debug, info, warn};

pub async fn send_transaction_impl(
    write_deps: &WriteDeps,
    transaction: String,
    _config: Option<RpcSendTransactionConfig>,
) -> RpcResult<String> {
    // Decode the base64 transaction
    let tx_data = STANDARD
        .decode(&transaction)
        .map_err(|e| custom_error(-32602, format!("Invalid base64 encoding: {}", e)))?;

    // Check packet size limit (1232 bytes is Solana's PACKET_DATA_SIZE)
    const PACKET_DATA_SIZE: usize = 1232;
    if tx_data.len() > PACKET_DATA_SIZE {
        return Err(custom_error(
            -32602,
            format!(
                "Transaction too large: {} bytes (max: {} bytes)",
                tx_data.len(),
                PACKET_DATA_SIZE
            ),
        ));
    }

    // Use bincode options matching Agave's decode_and_deserialize
    let bincode_options = bincode::options()
        .with_limit(PACKET_DATA_SIZE as u64)
        .with_fixint_encoding()
        .allow_trailing_bytes();

    // Try to deserialize as VersionedTransaction first (standard format)
    let versioned_tx = bincode_options
        .deserialize::<VersionedTransaction>(&tx_data)
        .map_err(|e| custom_error(-32602, format!("Failed to deserialize transaction: {}", e)))?;

    let runtime_tx = RuntimeTransaction::try_create(
        versioned_tx,
        MessageHash::Compute,
        None,
        SimpleAddressLoader::Enabled(LoadedAddresses {
            writable: vec![],
            readonly: vec![],
        }),
        &HashSet::new(),
    )
    .map_err(|err| custom_error(-32602, format!("invalid transaction: {err}")))?;
    let sanitized_tx = runtime_tx.into_inner_transaction();

    // Filter: only accept SPL token, ATA, System Program, and Withdraw Program transactions
    let is_allowed_transaction =
        sanitized_tx
            .message()
            .program_instructions_iter()
            .all(|(program_id, _)| {
                *program_id == spl_token::id()
                    || *program_id == spl_associated_token_account::id()
                    || *program_id == solana_sdk::system_program::id()
                    || *program_id == contra_withdraw_program_client::CONTRA_WITHDRAW_PROGRAM_ID
            });

    if !is_allowed_transaction {
        // Log which programs were found in the transaction
        let program_ids: Vec<String> = sanitized_tx
            .message()
            .program_instructions_iter()
            .map(|(program_id, _)| program_id.to_string())
            .collect();
        warn!(
            "Rejected transaction {}: programs used: {:?}",
            sanitized_tx.signature(),
            program_ids
        );
        return Err(custom_error(
            -32602,
            "Only SPL token, ATA, System, and Withdraw program transactions are accepted",
        ));
    }

    // Get the signature before sending to channel
    let signature = sanitized_tx.signature().to_string();

    // Send to dedup channel (which forwards to sigverify after deduplication)
    info!("Sending transaction {} to dedup stage", signature);
    write_deps
        .dedup_tx
        .send(sanitized_tx)
        .map_err(|_| custom_error(-32000, "Internal error: dedup channel closed"))?;

    debug!("Transaction {} sent to dedup stage", signature);
    Ok(signature)
}
