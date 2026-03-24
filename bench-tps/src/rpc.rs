//! Low-level async RPC helpers shared between the setup and load phases.
//!
//! These helpers abstract the two common patterns used throughout the bench:
//!   1. Sending many transactions in parallel (setup: ATA creation, mint-to)
//!   2. Polling `getSignatureStatuses` until every transaction settles

use {
    crate::types::{CONFIRM_TIMEOUT, MAX_CONCURRENT_SENDS, POLL_INTERVAL},
    anyhow::{Context, Result},
    futures::future::join_all,
    solana_client::nonblocking::rpc_client::RpcClient,
    solana_sdk::{signature::Keypair, signature::Signature},
    std::sync::Arc,
    tracing::warn,
};

// Re-export `info` so callers in this module can use it without an extra import.
use tracing::info;

/// Send one transaction per keypair concurrently, in chunks of
/// `MAX_CONCURRENT_SENDS`.
///
/// The `build` closure receives a keypair and a freshly cloned RPC URL and
/// returns a future that produces the transaction signature or a client error.
/// Each chunk of futures is awaited together via `join_all`; errors are logged
/// and discarded so that a single failure does not abort the whole batch.
///
/// Returns all successful signatures.
pub async fn send_parallel<F, Fut>(
    rpc_url: &str,
    keypairs: &[Arc<Keypair>],
    build: F,
) -> Vec<Signature>
where
    F: Fn(&Arc<Keypair>, String) -> Fut,
    Fut: std::future::Future<Output = Result<Signature, solana_client::client_error::ClientError>>,
{
    let mut sigs = Vec::with_capacity(keypairs.len());

    for chunk in keypairs.chunks(MAX_CONCURRENT_SENDS) {
        let futures: Vec<_> = chunk
            .iter()
            .map(|kp| build(kp, rpc_url.to_owned()))
            .collect();
        for res in join_all(futures).await {
            match res {
                Ok(sig) => sigs.push(sig),
                Err(e) => warn!(err = %e, "send failed"),
            }
        }
    }

    sigs
}

/// Poll `getSignatureStatuses` in a loop until every signature in `sigs` has
/// either been confirmed (no error) or failed (has an error), or until
/// `CONFIRM_TIMEOUT` elapses.
///
/// # Why not `send_and_confirm_transaction`?
///
/// The contra node settles transactions asynchronously through its 5-stage
/// pipeline.  The built-in confirmation timeout in `solana-client` is tied to
/// blockhash expiry (~60 s), which fires long before the pipeline finishes.
/// This function uses a generous 120 s timeout and polls at 500 ms intervals,
/// matching the actual settlement cadence.
pub async fn poll_confirmations(rpc: &RpcClient, sigs: &[Signature], label: &str) -> Result<()> {
    if sigs.is_empty() {
        return Ok(());
    }

    let deadline = tokio::time::Instant::now() + CONFIRM_TIMEOUT;

    loop {
        let statuses = rpc
            .get_signature_statuses(sigs)
            .await
            .context("get_signature_statuses")?
            .value;

        // Count outcomes: None = still pending, Some(status) with err = failed,
        // Some(status) without err = confirmed.
        let confirmed = statuses
            .iter()
            .filter(|s| s.as_ref().map_or(false, |s| s.err.is_none()))
            .count();
        let failed = statuses
            .iter()
            .filter(|s| s.as_ref().map_or(false, |s| s.err.is_some()))
            .count();
        let pending = sigs.len() - confirmed - failed;

        info!(label, confirmed, failed, pending, "Polling confirmations");

        if pending == 0 {
            if failed > 0 {
                warn!(label, failed, "Some transactions failed during confirmation");
            }
            return Ok(());
        }

        if tokio::time::Instant::now() >= deadline {
            return Err(anyhow::anyhow!(
                "Timeout waiting for {label} confirmations: {confirmed}/{} confirmed, {failed} failed",
                sigs.len()
            ));
        }

        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
