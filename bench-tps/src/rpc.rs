//! Low-level async RPC helpers shared between the setup and load phases.
//!
//! These helpers abstract the two common patterns used throughout the bench:
//!   1. Sending many transactions in parallel (setup: ATA creation, mint-to)
//!   2. Polling `getSignatureStatuses` until every transaction settles

use {
    crate::types::{CONFIRM_TIMEOUT, MAX_CONCURRENT_SENDS, POLL_INTERVAL, SIG_STATUS_CHUNK_SIZE},
    anyhow::{Context, Result},
    futures::future::join_all,
    solana_client::nonblocking::rpc_client::RpcClient,
    solana_sdk::{hash::Hash, signature::Keypair, signature::Signature},
    std::sync::Arc,
    tracing::{info, warn},
};

/// Send one transaction per keypair concurrently, in chunks of
/// `MAX_CONCURRENT_SENDS`.
///
/// The caller is responsible for fetching the blockhash (once per setup batch)
/// and passing it in.  All transactions within the batch share that hash;
/// since a batch is at most `SETUP_BATCH_SIZE` = 200 accounts and 200/64 × ~10 ms
/// ≈ 30 ms total send time, the hash cannot expire mid-batch.
///
/// Returns `Vec<Option<Signature>>` indexed 1:1 with `keypairs`:
///   - `Some(sig)` — `send_transaction` succeeded; sig is awaiting confirmation
///   - `None`      — `send_transaction` failed; this keypair index will be
///     returned by `poll_confirmations` as a retry candidate
///
/// Progress is logged after every chunk of 64 so long-running batches are not
/// silent.
pub async fn send_parallel<F, Fut>(
    rpc_url: &str,
    keypairs: &[Arc<Keypair>],
    blockhash: Hash,
    label: &str,
    build: F,
) -> Vec<Option<Signature>>
where
    F: Fn(&Arc<Keypair>, String, Hash) -> Fut,
    Fut: std::future::Future<Output = Result<Signature, solana_client::client_error::ClientError>>,
{
    let total = keypairs.len();
    // Pre-fill with None; successful sends overwrite their slot.
    let mut results: Vec<Option<Signature>> = vec![None; total];
    let mut sent = 0usize;

    for (chunk_start, chunk) in keypairs.chunks(MAX_CONCURRENT_SENDS).enumerate() {
        let offset = chunk_start * MAX_CONCURRENT_SENDS;
        let futures: Vec<_> = chunk
            .iter()
            .map(|kp| build(kp, rpc_url.to_owned(), blockhash))
            .collect();
        for (i, res) in join_all(futures).await.into_iter().enumerate() {
            match res {
                Ok(sig) => results[offset + i] = Some(sig),
                Err(e) => warn!(err = %e, label, "send_transaction failed"),
            }
        }

        sent += chunk.len();
        info!(label, sent, total, "send_parallel progress");
    }

    results
}

/// Poll `getSignatureStatuses` until every successfully-sent transaction has
/// either confirmed or failed, or until `CONFIRM_TIMEOUT` elapses.
///
/// Input `sigs` is indexed 1:1 with the keypair slice passed to `send_parallel`:
///   - `None`      — send_transaction failed; treated as an immediate retry
///   - `Some(sig)` — polled until confirmed, failed on-chain, or timed out
///
/// Returns the indices of all entries that need to be retried with a fresh
/// blockhash:
///   - `None` entries (HTTP send failure)
///   - `Some` entries whose transaction was rejected on-chain
///   - `Some` entries still pending after `CONFIRM_TIMEOUT`
///
/// Callers can map the returned indices back to their original keypair slice
/// to rebuild and resend only the affected transactions.
pub async fn poll_confirmations(
    rpc: &RpcClient,
    sigs: &[Option<Signature>],
    label: &str,
    // How many accounts were fully confirmed before this call (prior batches).
    // Combined with the count confirmed inside this call, produces a running
    // total so the log always shows progress against the overall account count.
    confirmed_before: usize,
    total: usize,
) -> Result<Vec<usize>> {
    // Entries that were never sent are immediate retry candidates.
    let mut retry_indices: Vec<usize> = sigs
        .iter()
        .enumerate()
        .filter_map(|(i, s)| s.is_none().then_some(i))
        .collect();

    // Track (original_index, signature) for the sigs that are still in flight.
    let mut pending: Vec<(usize, Signature)> = sigs
        .iter()
        .enumerate()
        .filter_map(|(i, s)| s.map(|sig| (i, sig)))
        .collect();

    if pending.is_empty() {
        return Ok(retry_indices);
    }

    let deadline = tokio::time::Instant::now() + CONFIRM_TIMEOUT;
    // Running total of accounts confirmed within this call across all poll ticks.
    let mut confirmed_this_call = 0usize;

    loop {
        let mut still_pending: Vec<(usize, Signature)> = Vec::new();
        let mut confirmed_count = 0usize;
        let mut failed_count = 0usize;

        // getSignatureStatuses is capped at SIG_STATUS_CHUNK_SIZE (256) per call.
        for chunk in pending.chunks(SIG_STATUS_CHUNK_SIZE) {
            let chunk_sigs: Vec<Signature> = chunk.iter().map(|(_, s)| *s).collect();
            let statuses = rpc
                .get_signature_statuses(&chunk_sigs)
                .await
                .context("get_signature_statuses")?
                .value;

            for (status, &(orig_idx, sig)) in statuses.iter().zip(chunk.iter()) {
                match status {
                    // Not yet visible to the RPC node — keep polling.
                    None => still_pending.push((orig_idx, sig)),
                    // On-chain execution error — retry with a new transaction.
                    Some(s) if s.err.is_some() => {
                        failed_count += 1;
                        retry_indices.push(orig_idx);
                    }
                    // Confirmed with no error — done.
                    Some(_) => confirmed_count += 1,
                }
            }
        }

        confirmed_this_call += confirmed_count;
        let accounts_confirmed = confirmed_before + confirmed_this_call;
        let accounts_remaining = total.saturating_sub(accounts_confirmed);
        info!(
            label,
            confirmed = confirmed_count,
            failed = failed_count,
            pending = still_pending.len(),
            accounts_confirmed,
            accounts_remaining,
            total,
            "poll_confirmations",
        );

        pending = still_pending;

        if pending.is_empty() {
            break;
        }

        if tokio::time::Instant::now() >= deadline {
            // Still-pending transactions are assumed expired (blockhash too old).
            // The caller will rebuild them with a fresh blockhash.
            warn!(
                label,
                timed_out = pending.len(),
                "Confirmation timeout — transactions will be retried with fresh blockhash",
            );
            retry_indices.extend(pending.iter().map(|(i, _)| i));
            break;
        }

        tokio::time::sleep(POLL_INTERVAL).await;
    }

    Ok(retry_indices)
}
