use {
    crate::{
        nodes::node::WorkerHandle,
        scheduler::{ConflictFreeBatch, Scheduler, SchedulerTrait},
        stage_metrics::SharedMetrics,
    },
    solana_sdk::transaction::SanitizedTransaction,
    tokio::sync::mpsc,
    tokio_util::sync::CancellationToken,
    tracing::{debug, info, warn},
};

pub struct SequencerArgs {
    pub max_tx_per_batch: usize,
    pub rx: mpsc::UnboundedReceiver<SanitizedTransaction>,
    pub batch_tx: mpsc::UnboundedSender<ConflictFreeBatch>,
    pub shutdown_token: CancellationToken,
    pub metrics: SharedMetrics,
}

pub async fn start_sequence_worker(args: SequencerArgs) -> WorkerHandle {
    let SequencerArgs {
        max_tx_per_batch,
        mut rx,
        batch_tx,
        shutdown_token,
        metrics,
    } = args;
    let handle = tokio::spawn(async move {
        info!(
            "Sequencer started with max_tx_per_batch: {}",
            max_tx_per_batch
        );

        let mut scheduler = Scheduler::new_dag();
        let mut pending_transactions = Vec::new();
        let mut total_batches_sent = 0u64;

        loop {
            // Collect transactions up to max_tx_per_batch or until channel is empty
            let mut collected = 0;

            // First, try to get at least one transaction (blocking)
            tokio::select! {
                result = rx.recv() => {
                    match result {
                        Some(transaction) => {
                            debug!("Sequencer received transaction: {}", transaction.signature());
                            pending_transactions.push(transaction);
                            collected += 1;
                        }
                        None => {
                            // Channel closed - process any remaining and exit
                            if !pending_transactions.is_empty() {
                                metrics.sequencer_collected(pending_transactions.len());
                                let sent = process_and_send_batches(
                                    &mut scheduler,
                                    &pending_transactions,
                                    &batch_tx,
                                    &metrics,
                                );
                                total_batches_sent += sent;
                            }
                            info!("Sequencer stopped - channel closed, sent {} total batches", total_batches_sent);
                            return;
                        }
                    }
                }

                _ = shutdown_token.cancelled() => {
                    // Process remaining transactions before shutdown
                    if !pending_transactions.is_empty() {
                        metrics.sequencer_collected(pending_transactions.len());
                        let sent = process_and_send_batches(
                            &mut scheduler,
                            &pending_transactions,
                            &batch_tx,
                            &metrics,
                        );
                        total_batches_sent += sent;
                    }
                    info!("Sequencer received shutdown signal, sent {} total batches", total_batches_sent);
                    return;
                }
            }

            // Now collect more transactions without blocking until we hit the limit or channel is empty
            while collected < max_tx_per_batch {
                match rx.try_recv() {
                    Ok(transaction) => {
                        debug!(
                            "Sequencer received transaction: {}",
                            transaction.signature()
                        );
                        pending_transactions.push(transaction);
                        collected += 1;
                    }
                    Err(_) => {
                        // Channel is empty (but not closed)
                        debug!("Channel empty after collecting {} transactions", collected);
                        break;
                    }
                }
            }

            if collected >= max_tx_per_batch {
                debug!("Reached max_tx_per_batch limit: {}", max_tx_per_batch);
            }

            // Process the collected transactions into conflict-free batches
            if !pending_transactions.is_empty() {
                metrics.sequencer_collected(pending_transactions.len());
                let sent =
                    process_and_send_batches(&mut scheduler, &pending_transactions, &batch_tx, &metrics);
                total_batches_sent += sent;
                pending_transactions.clear();

                if total_batches_sent.is_multiple_of(100) && total_batches_sent > 0 {
                    info!("Sequencer has sent {} total batches", total_batches_sent);
                }
            }
        }
    });

    WorkerHandle::new("Sequencer".to_string(), handle)
}

/// Visible to tests in this crate.
fn process_and_send_batches(
    scheduler: &mut Scheduler,
    transactions: &[SanitizedTransaction],
    batch_tx: &mpsc::UnboundedSender<ConflictFreeBatch>,
    metrics: &SharedMetrics,
) -> u64 {
    let num_transactions = transactions.len();
    debug!(
        "Processing {} transactions into conflict-free batches",
        num_transactions
    );

    // Schedule transactions to create conflict-free batches
    let conflict_free_batches = scheduler.schedule(transactions.to_vec());
    let num_batches = conflict_free_batches.len();

    if num_transactions > 0 {
        metrics.sequencer_transactions_emitted(num_transactions);
    }

    debug!(
        "Created {} conflict-free batches from {} transactions",
        num_batches, num_transactions
    );

    let mut batches_sent = 0u64;

    // Send each conflict-free batch to the executor
    for (idx, batch) in conflict_free_batches.into_iter().enumerate() {
        let batch_size = batch.transactions.len();
        debug!(
            "Sending conflict-free batch {} with {} transactions",
            idx, batch_size
        );

        match batch_tx.send(batch) {
            Ok(_) => {
                debug!("Batch {} sent successfully", idx);
                batches_sent += 1;
            }
            Err(_) => {
                warn!("Failed to send batch {} - channel closed", idx);
                break;
            }
        }
    }

    batches_sent
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{stage_metrics::NoopMetrics, test_helpers::create_test_sanitized_transaction};
    use solana_sdk::pubkey::Pubkey;
    use solana_sdk::signature::Keypair;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn test_single_tx_produces_batch() {
        let mut scheduler = Scheduler::new_dag();
        let (batch_tx, mut batch_rx) = mpsc::unbounded_channel();

        let from = Keypair::new();
        let to = Pubkey::new_unique();
        let tx = create_test_sanitized_transaction(&from, &to, 100);

        let noop: SharedMetrics = Arc::new(NoopMetrics);
        let sent = process_and_send_batches(&mut scheduler, &[tx], &batch_tx, &noop);
        assert!(sent >= 1);

        // Should have received at least one batch
        let batch = batch_rx.try_recv();
        assert!(batch.is_ok());
        assert!(!batch.unwrap().transactions.is_empty());
    }

    #[test]
    fn test_empty_no_batches() {
        let mut scheduler = Scheduler::new_dag();
        let (batch_tx, mut batch_rx) = mpsc::unbounded_channel();

        let noop: SharedMetrics = Arc::new(NoopMetrics);
        let sent = process_and_send_batches(&mut scheduler, &[], &batch_tx, &noop);
        assert_eq!(sent, 0);
        assert!(batch_rx.try_recv().is_err());
    }

    #[test]
    fn test_channel_closed_partial() {
        let mut scheduler = Scheduler::new_dag();
        let (batch_tx, batch_rx) = mpsc::unbounded_channel();

        // Drop the receiver so sends will fail after the first
        drop(batch_rx);

        let from = Keypair::new();
        let to = Pubkey::new_unique();
        let tx = create_test_sanitized_transaction(&from, &to, 100);

        // Should not panic, just return 0 since channel is closed
        let noop: SharedMetrics = Arc::new(NoopMetrics);
        let sent = process_and_send_batches(&mut scheduler, &[tx], &batch_tx, &noop);
        assert_eq!(sent, 0);
    }

    // Conflicting txs (same payer = write conflict) are split across separate conflict-free batches.
    #[test]
    fn test_multiple_txs_produce_multiple_batches() {
        // When transactions conflict they are split into separate batches.
        // Use the same payer (write conflict on fee payer account).
        let mut scheduler = Scheduler::new_dag();
        let (batch_tx, mut batch_rx) = mpsc::unbounded_channel();

        let payer = Keypair::new();
        let to1 = Pubkey::new_unique();
        let to2 = Pubkey::new_unique();
        let tx1 = create_test_sanitized_transaction(&payer, &to1, 100);
        let tx2 = create_test_sanitized_transaction(&payer, &to2, 200);

        let noop: SharedMetrics = Arc::new(NoopMetrics);
        let sent = process_and_send_batches(&mut scheduler, &[tx1, tx2], &batch_tx, &noop);
        // Conflicting transactions should be split into separate batches
        assert_eq!(
            sent, 2,
            "Two conflicting txs should produce two separate batches"
        );

        // Verify first batch received
        let batch1 = batch_rx.try_recv();
        assert!(batch1.is_ok(), "First batch should be received");
        assert_eq!(
            batch1.unwrap().transactions.len(),
            1,
            "First batch should contain one transaction"
        );

        // Verify second batch received
        let batch2 = batch_rx.try_recv();
        assert!(batch2.is_ok(), "Second batch should be received");
        assert_eq!(
            batch2.unwrap().transactions.len(),
            1,
            "Second batch should contain one transaction"
        );
    }

    // Txs with no shared accounts are eligible to be placed in the same batch.
    #[test]
    fn test_non_conflicting_txs_may_share_batch() {
        // Transactions with no shared accounts can be in the same batch.
        // Different payers and recipients = no conflicts = can share batch.
        let mut scheduler = Scheduler::new_dag();
        let (batch_tx, mut batch_rx) = mpsc::unbounded_channel();

        let from1 = Keypair::new();
        let from2 = Keypair::new();
        let to1 = Pubkey::new_unique();
        let to2 = Pubkey::new_unique();
        let tx1 = create_test_sanitized_transaction(&from1, &to1, 100);
        let tx2 = create_test_sanitized_transaction(&from2, &to2, 200);

        let noop: SharedMetrics = Arc::new(NoopMetrics);
        let sent = process_and_send_batches(&mut scheduler, &[tx1, tx2], &batch_tx, &noop);
        assert_eq!(
            sent, 1,
            "Non-conflicting txs should be grouped into one batch"
        );

        // Verify the batch contains both transactions
        let batch = batch_rx.try_recv();
        assert!(batch.is_ok(), "One batch should be received");
        assert_eq!(
            batch.unwrap().transactions.len(),
            2,
            "Batch should contain both non-conflicting transactions"
        );
    }

    // ---- start_sequence_worker tests ----

    // Closing the input channel with a pending tx causes the worker to flush it then exit.
    #[tokio::test]
    async fn worker_channel_closed_flushes_pending_and_exits() {
        let (input_tx, input_rx) = mpsc::unbounded_channel();
        let (batch_tx, mut batch_rx) = mpsc::unbounded_channel();
        let shutdown = CancellationToken::new();

        let from = Keypair::new();
        let to = Pubkey::new_unique();
        input_tx
            .send(create_test_sanitized_transaction(&from, &to, 100))
            .unwrap();
        drop(input_tx); // close the channel with a pending tx

        let _handle = start_sequence_worker(SequencerArgs {
            max_tx_per_batch: 64,
            rx: input_rx,
            batch_tx,
            shutdown_token: shutdown.clone(),
            metrics: Arc::new(NoopMetrics),
        })
        .await;

        // Worker should receive the pending tx, process it, then exit
        let result = tokio::time::timeout(Duration::from_millis(300), batch_rx.recv()).await;
        assert!(
            result.is_ok(),
            "batch should arrive before channel-close exit"
        );
        shutdown.cancel();
    }

    // Cancelling the shutdown token stops the worker without deadlock or panic.
    #[tokio::test]
    async fn worker_shutdown_signal_exits_cleanly() {
        let (input_tx, input_rx) = mpsc::unbounded_channel();
        let (batch_tx, mut batch_rx) = mpsc::unbounded_channel();
        let shutdown = CancellationToken::new();

        let _handle = start_sequence_worker(SequencerArgs {
            max_tx_per_batch: 64,
            rx: input_rx,
            batch_tx,
            shutdown_token: shutdown.clone(),
            metrics: Arc::new(NoopMetrics),
        })
        .await;

        // Send a tx so the worker has something to flush on shutdown
        let from = Keypair::new();
        let to = Pubkey::new_unique();
        input_tx
            .send(create_test_sanitized_transaction(&from, &to, 100))
            .unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;
        shutdown.cancel();

        // The batch emitted before or at shutdown should be receivable
        let _ = tokio::time::timeout(Duration::from_millis(200), batch_rx.recv()).await;
        // No panic or deadlock is the primary assertion here
        drop(input_tx);
    }

    // The worker's non-blocking drain loop stops collecting once max_tx_per_batch is reached.
    #[tokio::test]
    async fn worker_collects_up_to_max_tx_per_batch() {
        let (input_tx, input_rx) = mpsc::unbounded_channel();
        let (batch_tx, mut batch_rx) = mpsc::unbounded_channel();
        let shutdown = CancellationToken::new();
        let max = 3usize;
        let num_to_send = max * 2; // 6 items, more than max (3)

        // Pre-fill with more than max transactions so the non-blocking loop
        // hits the limit and breaks.
        for _ in 0..num_to_send {
            let from = Keypair::new();
            let to = Pubkey::new_unique();
            input_tx
                .send(create_test_sanitized_transaction(&from, &to, 100))
                .unwrap();
        }

        let _handle = start_sequence_worker(SequencerArgs {
            max_tx_per_batch: max,
            rx: input_rx,
            batch_tx,
            shutdown_token: shutdown.clone(),
            metrics: Arc::new(NoopMetrics),
        })
        .await;

        // Use timeout + recv instead of sleep + try_recv for determinism
        let result = tokio::time::timeout(Duration::from_millis(500), batch_rx.recv()).await;
        assert!(result.is_ok(), "expected at least one batch within timeout");
        let batch = result.unwrap().expect("channel should not be closed");
        assert_eq!(
            batch.transactions.len(),
            max,
            "Batch should contain exactly max_tx_per_batch ({}) transactions",
            max
        );
        shutdown.cancel();
    }
}
