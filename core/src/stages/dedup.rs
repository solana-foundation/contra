use {
    crate::nodes::node::WorkerHandle,
    solana_sdk::{hash::Hash, signature::Signature, transaction::SanitizedTransaction},
    std::collections::{HashMap, HashSet, LinkedList},
    tokio::sync::mpsc,
    tokio_util::sync::CancellationToken,
    tracing::{info, warn},
};

pub struct DedupArgs {
    pub max_blockhashes: usize,
    pub input_rx: mpsc::UnboundedReceiver<SanitizedTransaction>,
    pub settled_blockhashes_rx: mpsc::UnboundedReceiver<Hash>,
    pub output_tx: tokio_mpmc::Sender<SanitizedTransaction>,
    pub shutdown_token: CancellationToken,
}

/// Create the dedup channel pair (unbounded)
pub fn create_dedup_channel() -> (
    mpsc::UnboundedSender<SanitizedTransaction>,
    mpsc::UnboundedReceiver<SanitizedTransaction>,
) {
    mpsc::unbounded_channel()
}

pub async fn start_dedup(args: DedupArgs) -> WorkerHandle {
    let DedupArgs {
        max_blockhashes,
        mut input_rx,
        mut settled_blockhashes_rx,
        output_tx,
        shutdown_token,
    } = args;
    let handle = tokio::spawn(async move {
        info!("Dedup stage started");

        // HashMap: blockhash -> set of signatures
        let mut dedup_cache: HashMap<Hash, HashSet<Signature>> = HashMap::new();
        let mut live_blockhashes = LinkedList::new();

        loop {
            tokio::select! {
                // Process incoming settled blockhashes
                result = settled_blockhashes_rx.recv() => {
                    match result {
                        Some(blockhash) => {
                            live_blockhashes.push_back(blockhash);
                            while live_blockhashes.len() > max_blockhashes {
                                if let Some(expired_blockhash) = live_blockhashes.pop_front() {
                                    dedup_cache.remove(&expired_blockhash);
                                }
                            }
                        }
                        None => {
                            warn!("Dedup settled blockhashes channel closed, shutting down");
                            break;
                        }
                    }
                }
                // Process incoming transactions
                result = input_rx.recv() => {
                    match result {
                        Some(transaction) => {
                            let signature = *transaction.signature();
                            let blockhash = *transaction.message().recent_blockhash();

                            if !live_blockhashes.contains(&blockhash) {
                                warn!("Blockhash {} not found in live blockhashes", blockhash);
                                continue;
                            }

                            // Check if duplicate using two-layer lookup
                            let is_duplicate = dedup_cache
                                .get(&blockhash)
                                .map(|sigs| sigs.contains(&signature))
                                .unwrap_or(false);

                            if is_duplicate {
                                warn!("Duplicate transaction detected: {} (blockhash: {})", signature, blockhash);
                                // TODO: Track duplicate metrics
                                // TODO: Consider returning an error to the client
                                continue;
                            }

                            // Add to cache
                            dedup_cache
                                .entry(blockhash)
                                .or_default()
                                .insert(signature);

                            // Forward to sigverify
                            if let Err(e) = output_tx.send(transaction).await {
                                warn!("Failed to forward transaction to sigverify: {}", e);
                                break;
                            }
                        }
                        None => {
                            warn!("Dedup input channel closed, shutting down");
                            break;
                        }
                    }
                }

                // Shutdown signal
                _ = shutdown_token.cancelled() => {
                    info!("Dedup received shutdown signal");
                    break;
                }
            }
        }

        info!("Dedup stopped");
    });

    WorkerHandle::new("Dedup".to_string(), handle)
}
