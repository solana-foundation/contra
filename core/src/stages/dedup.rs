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

#[cfg(test)]
mod tests {
    use super::*;
    use solana_sdk::{
        hash::Hash,
        message::Message,
        pubkey::Pubkey,
        signature::{Keypair, Signer},
        transaction::{SanitizedTransaction, Transaction},
    };
    use solana_system_interface::instruction as system_instruction;
    use std::collections::HashSet;
    use std::time::Duration;

    fn make_tx(payer: &Keypair, blockhash: Hash) -> SanitizedTransaction {
        let to = Pubkey::new_unique();
        let ix = system_instruction::transfer(&payer.pubkey(), &to, 1);
        let msg = Message::new(&[ix], Some(&payer.pubkey()));
        let tx = Transaction::new(&[payer], msg, blockhash);
        SanitizedTransaction::try_from_legacy_transaction(tx, &HashSet::new()).unwrap()
    }

    /// Spin up the dedup stage and return the handles needed for driving it.
    fn start_test_dedup() -> (
        mpsc::UnboundedSender<SanitizedTransaction>,
        mpsc::UnboundedSender<Hash>,
        tokio_mpmc::Receiver<SanitizedTransaction>,
        CancellationToken,
    ) {
        let (input_tx, input_rx) = mpsc::unbounded_channel();
        let (bh_tx, bh_rx) = mpsc::unbounded_channel();
        let (output_tx, output_rx) = tokio_mpmc::channel(64);
        let shutdown = CancellationToken::new();

        let args = DedupArgs {
            max_blockhashes: 8,
            input_rx,
            settled_blockhashes_rx: bh_rx,
            output_tx,
            shutdown_token: shutdown.clone(),
        };
        tokio::spawn(async move {
            start_dedup(args).await;
        });

        (input_tx, bh_tx, output_rx, shutdown)
    }

    // --- C4: transaction with unknown blockhash must be rejected --------

    #[tokio::test]
    async fn unknown_blockhash_rejected() {
        let (input_tx, bh_tx, output_rx, shutdown) = start_test_dedup();

        // Register one live blockhash
        let live_bh = Hash::new_unique();
        bh_tx.send(live_bh).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        // Send tx with a *different* blockhash
        let payer = Keypair::new();
        let unknown_bh = Hash::new_unique();
        let tx = make_tx(&payer, unknown_bh);
        input_tx.send(tx).unwrap();

        // Should NOT appear on the output channel
        let result = tokio::time::timeout(Duration::from_millis(100), output_rx.recv()).await;
        assert!(
            result.is_err(),
            "tx with unknown blockhash should not be forwarded"
        );

        shutdown.cancel();
    }

    // --- C3: duplicate signature must be rejected ----------------------

    #[tokio::test]
    async fn duplicate_signature_rejected() {
        let (input_tx, bh_tx, output_rx, shutdown) = start_test_dedup();

        let bh = Hash::new_unique();
        bh_tx.send(bh).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let payer = Keypair::new();
        let tx = make_tx(&payer, bh);

        // First send — should be forwarded
        input_tx.send(tx.clone()).unwrap();
        let first = tokio::time::timeout(Duration::from_millis(200), output_rx.recv()).await;
        assert!(first.is_ok(), "first tx should be forwarded");

        // Second send (same signature) — should be dropped
        input_tx.send(tx).unwrap();
        let second = tokio::time::timeout(Duration::from_millis(100), output_rx.recv()).await;
        assert!(second.is_err(), "duplicate tx should not be forwarded");

        shutdown.cancel();
    }

    // --- Happy path: valid unique tx with known blockhash forwarded ----

    #[tokio::test]
    async fn valid_transaction_forwarded() {
        let (input_tx, bh_tx, output_rx, shutdown) = start_test_dedup();

        let bh = Hash::new_unique();
        bh_tx.send(bh).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let payer = Keypair::new();
        let tx = make_tx(&payer, bh);
        let expected_sig = *tx.signature();

        input_tx.send(tx).unwrap();

        let result = tokio::time::timeout(Duration::from_millis(200), output_rx.recv()).await;
        match result {
            Ok(Ok(Some(forwarded))) => {
                assert_eq!(*forwarded.signature(), expected_sig);
            }
            other => panic!("expected forwarded tx, got {:?}", other),
        }

        shutdown.cancel();
    }

    // --- Blockhash window eviction ------------------------------------

    #[tokio::test]
    async fn expired_blockhash_evicted() {
        let (input_tx, bh_tx, output_rx, shutdown) = start_test_dedup();

        // Fill the window (max_blockhashes = 8) then add one more to evict the first
        let mut hashes = Vec::new();
        for _ in 0..9 {
            let h = Hash::new_unique();
            hashes.push(h);
            bh_tx.send(h).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(30)).await;

        // hashes[0] should now be evicted
        let payer = Keypair::new();
        let tx = make_tx(&payer, hashes[0]);
        input_tx.send(tx).unwrap();
        let result = tokio::time::timeout(Duration::from_millis(100), output_rx.recv()).await;
        assert!(
            result.is_err(),
            "tx using evicted blockhash should not be forwarded"
        );

        // hashes[8] (latest) should still work
        let tx2 = make_tx(&payer, hashes[8]);
        input_tx.send(tx2).unwrap();
        let result2 = tokio::time::timeout(Duration::from_millis(200), output_rx.recv()).await;
        assert!(
            result2.is_ok(),
            "tx using latest blockhash should be forwarded"
        );

        shutdown.cancel();
    }
}
