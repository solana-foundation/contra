//! `test_send_transaction_error_classification`
//!
//! Target file: `core/src/rpc/send_transaction_impl.rs`.
//! Binary: `private_channel_integration` (existing).
//! Fixture: reuses `PrivateChannelContext`.
//!
//! Covers the two non-SDK-duplication branches in `send_transaction_impl`:
//!
//!   A. **Base64 decode failure** — SDK `send_transaction` does client-side
//!      pre-encoding, so an entirely-invalid-base64 case doesn't reach the
//!      server. We therefore use the lower-level `send::<T>(RpcRequest::
//!      SendTransaction, ...)` path and pass a string we know base64 cannot
//!      decode. Hits the base64-decode error arm.
//!
//!   B. **Oversized transaction** — constructs a binary blob >
//!      `PACKET_DATA_SIZE` (1232 bytes), base64-encodes it, and sends. The
//!      server must reject with `INVALID_PARAMS_CODE` before the pipeline
//!      is entered. Hits the size-check arm.
//!
//!   C. **System instruction not in allowlist** — admission is per instruction,
//!      not per program: the System program is limited to `Transfer`. A signed
//!      `Allocate` must be rejected at ingress (C1) and must leave no account
//!      behind (C2), which is the end-to-end no-persistent-state invariant.

use {
    super::test_context::PrivateChannelContext,
    base64::{engine::general_purpose::STANDARD, Engine as _},
    serde_json::json,
    solana_client::rpc_request::RpcRequest,
    solana_sdk::{
        commitment_config::CommitmentConfig,
        signature::{Keypair, Signer},
        transaction::Transaction,
    },
    solana_system_interface::instruction as system_instruction,
};

const INVALID_PARAMS_CODE: i64 = -32_602;

pub async fn run_send_transaction_errors_test(ctx: &PrivateChannelContext) {
    println!("\n=== sendTransaction — Error Classification ===");

    case_a_base64_decode_failure(ctx).await;
    case_b_oversized_transaction(ctx).await;
    case_c_system_allocate_rejected(ctx).await;

    println!("✓ base64-decode + oversized + System-allocate branches passed");
}

// ── Case A ──────────────────────────────────────────────────────────────────
async fn case_a_base64_decode_failure(ctx: &PrivateChannelContext) {
    // A string that STANDARD engine cannot decode (invalid chars + bad padding).
    // Sent as raw `SendTransaction` params to bypass client-side pre-encoding.
    let bad = "!!not-base64!!";
    let err = ctx
        .write_client
        .send::<serde_json::Value>(
            RpcRequest::SendTransaction,
            json!([bad, {"skipPreflight": true, "encoding": "base64"}]),
        )
        .await
        .expect_err("invalid base64 must be rejected by the server");

    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("base64")
            || msg.contains("invalid")
            || msg.contains(&INVALID_PARAMS_CODE.to_string()),
        "error must name base64/invalid-param as cause; got: {msg}"
    );
}

// ── Case B ──────────────────────────────────────────────────────────────────
async fn case_b_oversized_transaction(ctx: &PrivateChannelContext) {
    // PACKET_DATA_SIZE = 1232; send 1233 bytes of junk — valid base64, but
    // the decoded length exceeds the packet limit so the handler rejects
    // before attempting bincode deserialization.
    let junk = vec![0u8; 1233];
    let encoded = STANDARD.encode(&junk);
    let err = ctx
        .write_client
        .send::<serde_json::Value>(
            RpcRequest::SendTransaction,
            json!([encoded, {"skipPreflight": true, "encoding": "base64"}]),
        )
        .await
        .expect_err("oversized tx must be rejected");

    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("too large") || msg.contains("1232") || msg.contains("1233"),
        "error must identify size as the cause; got: {msg}"
    );
}

// ── Case C ──────────────────────────────────────────────────────────────────
async fn case_c_system_allocate_rejected(ctx: &PrivateChannelContext) {
    let payer = Keypair::new();
    let fresh = Keypair::new();
    let blockhash = ctx
        .get_blockhash()
        .await
        .expect("blockhash for the allocate tx");

    // `allocate` marks its account as a signer, so `fresh` must sign too or the
    // tx fails sanitization and this case would pass for the wrong reason.
    let ix = system_instruction::allocate(&fresh.pubkey(), 10 * 1024 * 1024);
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[&payer, &fresh],
        blockhash,
    );

    // C1: the RPC surface itself rejects it, not just the unit-level predicate.
    let err = ctx
        .write_client
        .send_transaction(&tx)
        .await
        .expect_err("System Allocate must be rejected at ingress");
    let msg = err.to_string();
    assert!(
        msg.contains("Only SPL token") || msg.contains(&INVALID_PARAMS_CODE.to_string()),
        "error must name the allowlist as the cause; got: {msg}"
    );

    // C2: the invariant the issue is about, no account row is created.
    let account = ctx
        .read_client
        .get_account_with_commitment(&fresh.pubkey(), CommitmentConfig::processed())
        .await
        .expect("get_account_with_commitment must succeed");
    assert!(
        account.value.is_none(),
        "a rejected allocate must leave no account behind"
    );
}
