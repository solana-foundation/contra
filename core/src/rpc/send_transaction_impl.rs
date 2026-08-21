use crate::rpc::{
    constants::PACKET_DATA_SIZE,
    error::{custom_error, node_at_capacity, INVALID_PARAMS_CODE, JSON_RPC_SERVER_ERROR},
    WriteDeps,
};
use crate::transactions::is_allowed_program_instruction;
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
    let tx_data = STANDARD.decode(&transaction).map_err(|e| {
        custom_error(
            INVALID_PARAMS_CODE,
            format!("Invalid base64 encoding: {}", e),
        )
    })?;

    if tx_data.len() > PACKET_DATA_SIZE {
        return Err(custom_error(
            INVALID_PARAMS_CODE,
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
        .map_err(|e| {
            custom_error(
                INVALID_PARAMS_CODE,
                format!("Failed to deserialize transaction: {}", e),
            )
        })?;

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
    .map_err(|err| custom_error(INVALID_PARAMS_CODE, format!("invalid transaction: {err}")))?;
    let sanitized_tx = runtime_tx.into_inner_transaction();

    // Admission is per instruction, not per program: System is limited to Transfer.
    let is_allowed_transaction = sanitized_tx
        .message()
        .program_instructions_iter()
        .all(|(program_id, ix)| is_allowed_program_instruction(program_id, &ix.data));

    if !is_allowed_transaction {
        // Name the program and its leading tag bytes so the log identifies the
        // offending instruction, not just the transaction.
        let offenders: Vec<String> = sanitized_tx
            .message()
            .program_instructions_iter()
            .map(|(program_id, ix)| {
                let tag = &ix.data[..ix.data.len().min(4)];
                let allowed = is_allowed_program_instruction(program_id, &ix.data);
                format!("{program_id} tag={tag:02x?} allowed={allowed}")
            })
            .collect();
        warn!(
            "Rejected transaction {}: instructions: {:?}",
            sanitized_tx.signature(),
            offenders
        );
        return Err(custom_error(
            INVALID_PARAMS_CODE,
            "Only SPL token, ATA, Memo, Withdraw, and Swap program transactions are accepted; System is limited to Transfer",
        ));
    }

    // Get the signature before sending to channel
    let signature = sanitized_tx.signature().to_string();

    // Fail fast on a full ingress queue: shedding frees the RPC connection slot
    // immediately rather than parking it, so a memory-DoS can't become a
    // connection-exhaustion DoS. The shed surfaces a distinct retryable code.
    info!("Sending transaction {} to dedup stage", signature);
    match write_deps.dedup_tx.try_send(sanitized_tx) {
        Ok(()) => {
            debug!("Transaction {} sent to dedup stage", signature);
            Ok(signature)
        }
        Err(async_channel::TrySendError::Full(_)) => {
            write_deps.metrics.rpc_ingress_shed();
            warn!("Shed transaction {}: ingress queue full", signature);
            Err(node_at_capacity())
        }
        Err(async_channel::TrySendError::Closed(_)) => Err(custom_error(
            JSON_RPC_SERVER_ERROR,
            "Internal error: ingress channel closed",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::{error::NODE_AT_CAPACITY_CODE, WriteDeps};
    use crate::stage_metrics::{NoopMetrics, PrometheusMetrics, SharedMetrics};
    use solana_sdk::{
        hash::Hash,
        instruction::Instruction,
        pubkey::Pubkey,
        signature::{Keypair, Signer},
        transaction::{SanitizedTransaction, Transaction},
    };
    use solana_system_interface::instruction as system_instruction;
    use std::sync::Arc;

    const TEST_INGRESS_CAP: usize = 4;

    fn encode_tx(tx: &Transaction) -> String {
        let bytes = bincode::serialize(tx).unwrap();
        STANDARD.encode(&bytes)
    }

    /// Returns WriteDeps and the receiver (must be held alive for happy-path tests).
    fn make_write_deps() -> (WriteDeps, async_channel::Receiver<SanitizedTransaction>) {
        make_write_deps_with(Arc::new(NoopMetrics))
    }

    fn make_write_deps_with(
        metrics: SharedMetrics,
    ) -> (WriteDeps, async_channel::Receiver<SanitizedTransaction>) {
        let (dedup_tx, rx) = async_channel::bounded(TEST_INGRESS_CAP);
        (WriteDeps { dedup_tx, metrics }, rx)
    }

    fn spl_tx() -> Transaction {
        let payer = Keypair::new();
        let from_ata = Pubkey::new_unique();
        let to_ata = Pubkey::new_unique();
        let ix = spl_token::instruction::transfer(
            &spl_token::id(),
            &from_ata,
            &to_ata,
            &payer.pubkey(),
            &[],
            1_000,
        )
        .unwrap();
        Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer], Hash::default())
    }

    #[tokio::test]
    async fn ingress_sheds_when_full() {
        let (deps, rx) = make_write_deps();
        // Fill to capacity without draining.
        for _ in 0..TEST_INGRESS_CAP {
            send_transaction_impl(&deps, encode_tx(&spl_tx()), None)
                .await
                .expect("accept until full");
        }

        let err = send_transaction_impl(&deps, encode_tx(&spl_tx()), None)
            .await
            .expect_err("a full ingress queue must shed");
        assert_eq!(err.code(), NODE_AT_CAPACITY_CODE);
        assert_eq!(
            rx.len(),
            TEST_INGRESS_CAP,
            "every accepted tx stays buffered, so the queue is saturated at the shed point"
        );
    }

    #[tokio::test]
    async fn ingress_shed_increments_metric() {
        let metrics: SharedMetrics = Arc::new(PrometheusMetrics);
        let (deps, _rx) = make_write_deps_with(Arc::clone(&metrics));
        for _ in 0..TEST_INGRESS_CAP {
            send_transaction_impl(&deps, encode_tx(&spl_tx()), None)
                .await
                .unwrap();
        }

        let before = shed_counter_value();
        let _ = send_transaction_impl(&deps, encode_tx(&spl_tx()), None).await;
        assert_eq!(
            shed_counter_value(),
            before + 1.0,
            "shed path must increment rpc_ingress_shed_total"
        );
    }

    // A shed happens at ingress, before the dedup cache insert, so the identical
    // tx can be resubmitted once capacity frees and is accepted (not rejected as
    // a duplicate). Proves the shed-before-dedup client-retry contract.
    #[tokio::test]
    async fn shed_tx_can_be_resubmitted() {
        let (deps, rx) = make_write_deps();
        for _ in 0..TEST_INGRESS_CAP {
            send_transaction_impl(&deps, encode_tx(&spl_tx()), None)
                .await
                .unwrap();
        }

        let tx = spl_tx();
        let encoded = encode_tx(&tx);
        let shed = send_transaction_impl(&deps, encoded.clone(), None).await;
        assert_eq!(shed.unwrap_err().code(), NODE_AT_CAPACITY_CODE);

        // Free capacity, then resubmit the identical tx — must be accepted.
        rx.recv().await.expect("drain one to free capacity");
        let resubmit = send_transaction_impl(&deps, encoded, None).await;
        assert!(
            resubmit.is_ok(),
            "a shed tx must be resubmittable: {resubmit:?}"
        );
    }

    fn shed_counter_value() -> f64 {
        private_channel_metrics::prometheus::gather()
            .into_iter()
            .filter(|mf| mf.name() == "private_channel_rpc_ingress_shed_total")
            .flat_map(|mf| mf.get_metric().to_vec())
            .map(|m| m.get_counter().value())
            .sum()
    }

    #[tokio::test]
    async fn disallowed_program_rejected() {
        let payer = Keypair::new();
        let fake_program = Pubkey::new_unique();
        let ix = Instruction {
            program_id: fake_program,
            accounts: vec![],
            data: vec![1],
        };
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer.pubkey()),
            &[&payer],
            Hash::default(),
        );
        let encoded = encode_tx(&tx);
        let (deps, _rx) = make_write_deps();

        let result = send_transaction_impl(&deps, encoded, None).await;
        assert!(result.is_err(), "disallowed program should be rejected");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Only SPL token"),
            "expected allowlist error, got: {err_msg}"
        );
    }

    #[tokio::test]
    async fn allowed_programs_accepted() {
        let payer = Keypair::new();
        let from_ata = Pubkey::new_unique();
        let to_ata = Pubkey::new_unique();
        let ix = spl_token::instruction::transfer(
            &spl_token::id(),
            &from_ata,
            &to_ata,
            &payer.pubkey(),
            &[],
            1_000,
        )
        .unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer.pubkey()),
            &[&payer],
            Hash::default(),
        );
        let encoded = encode_tx(&tx);
        // Keep _rx alive so the dedup channel send succeeds
        let (deps, _rx) = make_write_deps();

        let result = send_transaction_impl(&deps, encoded, None).await;
        assert!(result.is_ok(), "SPL token tx should pass allowlist");
    }

    #[tokio::test]
    async fn memo_program_accepted() {
        let payer = Keypair::new();
        let memo_ix = Instruction {
            program_id: spl_memo::id(),
            accounts: vec![],
            data: b"private_channel:mint-idempotency:42".to_vec(),
        };
        let tx = Transaction::new_signed_with_payer(
            &[memo_ix],
            Some(&payer.pubkey()),
            &[&payer],
            Hash::default(),
        );
        let encoded = encode_tx(&tx);
        let (deps, _rx) = make_write_deps();

        let result = send_transaction_impl(&deps, encoded, None).await;
        assert!(result.is_ok(), "Memo tx should pass allowlist");
    }

    // B1: CreateAccount allocates permanent account data that nothing charges
    // for and the tombstone rule never reclaims, so it must not reach ingress.
    #[tokio::test]
    async fn system_create_account_rejected() {
        let payer = Keypair::new();
        let fresh = Keypair::new();
        let ix = system_instruction::create_account(
            &payer.pubkey(),
            &fresh.pubkey(),
            0,
            10 * 1024 * 1024,
            &solana_sdk_ids::system_program::ID,
        );
        // create_account marks the new account as a signer, so both keys sign
        // or sanitization would reject the tx before the allowlist runs.
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer.pubkey()),
            &[&payer, &fresh],
            Hash::default(),
        );
        let (deps, rx) = make_write_deps();

        let err = send_transaction_impl(&deps, encode_tx(&tx), None)
            .await
            .expect_err("System CreateAccount must be rejected");
        assert_eq!(err.code(), INVALID_PARAMS_CODE);
        assert!(
            err.to_string().contains("Only SPL token"),
            "expected allowlist error, got: {err}"
        );
        assert!(
            rx.is_empty(),
            "rejected tx must not reach the ingress queue"
        );
    }

    // B2: Allocate is the cheapest shape of the same attack: one signer, no
    // lamport source, and the resulting 0-lamport account has non-empty data so
    // it can never be tombstoned.
    #[tokio::test]
    async fn system_allocate_rejected() {
        let payer = Keypair::new();
        let fresh = Keypair::new();
        let ix = system_instruction::allocate(&fresh.pubkey(), 10 * 1024 * 1024);
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer.pubkey()),
            &[&payer, &fresh],
            Hash::default(),
        );
        let (deps, rx) = make_write_deps();

        let err = send_transaction_impl(&deps, encode_tx(&tx), None)
            .await
            .expect_err("System Allocate must be rejected");
        assert_eq!(err.code(), INVALID_PARAMS_CODE);
        assert!(
            err.to_string().contains("Only SPL token"),
            "expected allowlist error, got: {err}"
        );
        assert!(
            rx.is_empty(),
            "rejected tx must not reach the ingress queue"
        );
    }

    // B3: System Transfer stays admitted, including alongside another allowed
    // program. Guards against an over-broad rewrite of the `.all(..)` filter.
    #[tokio::test]
    async fn system_transfer_with_memo_accepted() {
        let payer = Keypair::new();
        let transfer_ix =
            system_instruction::transfer(&payer.pubkey(), &Pubkey::new_unique(), 1_000);
        let memo_ix = Instruction {
            program_id: spl_memo::id(),
            accounts: vec![],
            data: b"private_channel:transfer".to_vec(),
        };
        let tx = Transaction::new_signed_with_payer(
            &[transfer_ix, memo_ix],
            Some(&payer.pubkey()),
            &[&payer],
            Hash::default(),
        );
        let (deps, rx) = make_write_deps();

        let result = send_transaction_impl(&deps, encode_tx(&tx), None).await;
        assert!(
            result.is_ok(),
            "System Transfer + Memo must pass: {result:?}"
        );
        assert_eq!(rx.len(), 1, "the accepted tx must reach the ingress queue");
    }

    // A closed ingress channel (receiver dropped) must surface the closed error,
    // not the retryable capacity shed. Guards the async_channel error mapping.
    #[tokio::test]
    async fn ingress_closed_yields_error() {
        let (deps, rx) = make_write_deps();
        drop(rx);

        let result = send_transaction_impl(&deps, encode_tx(&spl_tx()), None).await;
        let err = result.expect_err("a closed ingress channel must error");
        assert_ne!(
            err.code(),
            NODE_AT_CAPACITY_CODE,
            "closed is not a retryable capacity shed"
        );
        assert!(
            err.to_string().contains("ingress channel closed"),
            "expected closed-channel error, got: {err}"
        );
    }
}
