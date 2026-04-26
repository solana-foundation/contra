//! `test_send_transaction_error_classification`
//!
//! Target file: `core/src/rpc/send_transaction_impl.rs`.
//! Binary: `contra_integration` (existing).
//! Fixture: reuses `ContraContext`.
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
//! Case C ("program not in allowlist") is out of scope here because the
//! allowlist enforcement lives in a separate later stage and requires
//! configuration plumbing not part of the default `ContraContext`. That
//! branch can be a follow-up test when the context exposes a runtime
//! allowlist toggle.

use {
    super::test_context::ContraContext,
    base64::{engine::general_purpose::STANDARD, Engine as _},
    serde_json::json,
    solana_client::rpc_request::RpcRequest,
};

const INVALID_PARAMS_CODE: i64 = -32_602;

pub async fn run_send_transaction_errors_test(ctx: &ContraContext) {
    println!("\n=== sendTransaction — Error Classification ===");

    case_a_base64_decode_failure(ctx).await;
    case_b_oversized_transaction(ctx).await;

    println!("✓ base64-decode + oversized branches passed");
}

// ── Case A ──────────────────────────────────────────────────────────────────
async fn case_a_base64_decode_failure(ctx: &ContraContext) {
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
async fn case_b_oversized_transaction(ctx: &ContraContext) {
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
