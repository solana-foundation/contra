use {
    crate::{
        nodes::node::WorkerHandle,
        scheduler::{ConflictFreeBatch, Scheduler, SchedulerTrait},
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
}

pub async fn start_sequence_worker(args: SequencerArgs) -> WorkerHandle {
    let SequencerArgs {
        max_tx_per_batch,
        mut rx,
        batch_tx,
        shutdown_token,
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
                                let sent = process_and_send_batches(
                                    &mut scheduler,
                                    &pending_transactions,
                                    &batch_tx,
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
                        let sent = process_and_send_batches(
                            &mut scheduler,
                            &pending_transactions,
                            &batch_tx,
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
                let sent =
                    process_and_send_batches(&mut scheduler, &pending_transactions, &batch_tx);
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

fn process_and_send_batches(
    scheduler: &mut Scheduler,
    transactions: &[SanitizedTransaction],
    batch_tx: &mpsc::UnboundedSender<ConflictFreeBatch>,
) -> u64 {
    let num_transactions = transactions.len();
    debug!(
        "Processing {} transactions into conflict-free batches",
        num_transactions
    );

    // Schedule transactions to create conflict-free batches
    let conflict_free_batches = scheduler.schedule(transactions.to_vec());
    let num_batches = conflict_free_batches.len();

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
