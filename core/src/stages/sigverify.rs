// Signature verification stage for Contra

use {
    crate::{nodes::node::WorkerHandle, transactions::is_admin_instruction},
    solana_sdk::{pubkey::Pubkey, transaction::SanitizedTransaction},
    std::{
        fmt::{self, Display},
        sync::Arc,
    },
    tokio::sync::mpsc,
    tokio_mpmc,
    tokio_util::sync::CancellationToken,
    tracing::{debug, info, warn},
};

#[derive(Debug, Clone)]
pub enum SigverifyResult {
    Valid(TransactionType),
    InvalidTransaction(TransactionType),
    NotSignedByAdmin,
    SigverifyFailed(String),
}

impl Display for SigverifyResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SigverifyResult::Valid(transaction_type) => write!(f, "Valid: {:?}", transaction_type),
            SigverifyResult::InvalidTransaction(transaction_type) => {
                write!(f, "Invalid transaction: {:?}", transaction_type)
            }
            SigverifyResult::NotSignedByAdmin => write!(f, "Not signed by admin"),
            SigverifyResult::SigverifyFailed(e) => write!(f, "Sigverify failed: {}", e),
        }
    }
}

#[derive(Debug, Clone)]
pub enum TransactionType {
    /// Transaction contains no instructions
    Empty,
    /// Transaction contains only admin instructions
    Admin,
    /// Transaction contains only non-admin instructions
    Normal,
    /// Transaction contains both admin and non-admin instructions
    Mixed,
}

impl Display for TransactionType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransactionType::Empty => write!(f, "Empty"),
            TransactionType::Admin => write!(f, "Admin"),
            TransactionType::Normal => write!(f, "Normal"),
            TransactionType::Mixed => write!(f, "Mixed"),
        }
    }
}

/// Check if any signer is an admin
fn is_signed_by_admin(transaction: &SanitizedTransaction, admin_keys: &[Pubkey]) -> bool {
    let account_keys = transaction.message().account_keys();
    let num_required_signatures = transaction.message().header().num_required_signatures as usize;

    // All accounts before num_required_signatures are signers
    account_keys
        .iter()
        .take(num_required_signatures)
        .any(|pubkey| admin_keys.contains(pubkey))
}

/// Classifies a transaction into one TransactionType enum
fn classify_transaction(transaction: &SanitizedTransaction) -> TransactionType {
    let mut num_admin_ix = 0;
    let mut num_ix = 0;

    for (program_id, instruction) in transaction.message().program_instructions_iter() {
        // Get instruction type
        let instruction_type = match instruction.data.first() {
            Some(t) => *t,
            None => continue,
        };

        if is_admin_instruction(program_id, instruction_type) {
            num_admin_ix += 1;
        }

        num_ix += 1;
    }

    if num_ix == 0 {
        TransactionType::Empty
    } else if num_admin_ix == 0 {
        TransactionType::Normal
    } else if num_admin_ix == num_ix {
        TransactionType::Admin
    } else {
        TransactionType::Mixed
    }
}

pub struct SigverifyArgs {
    pub num_workers: usize,
    pub admin_keys: Vec<Pubkey>,
    pub rx: tokio_mpmc::Receiver<SanitizedTransaction>,
    pub sequencer_tx: mpsc::UnboundedSender<SanitizedTransaction>,
    pub shutdown_token: CancellationToken,
}

pub async fn sigverify_transaction(
    transaction: &SanitizedTransaction,
    admin_keys: &[Pubkey],
) -> SigverifyResult {
    let transaction_type = classify_transaction(transaction);

    // Check transaction type
    match transaction_type {
        TransactionType::Empty | TransactionType::Mixed => {
            return SigverifyResult::InvalidTransaction(transaction_type);
        }
        TransactionType::Admin => {
            // Validate that at least one of the signatures came from an admin pubkey
            if !is_signed_by_admin(transaction, admin_keys) {
                return SigverifyResult::NotSignedByAdmin;
            }
        }
        TransactionType::Normal => {}
    }

    // Verify signature
    match transaction.verify() {
        Ok(_) => SigverifyResult::Valid(transaction_type),
        Err(e) => SigverifyResult::SigverifyFailed(e.to_string()),
    }
}

/// Start the signature verification worker pool
pub async fn start_sigverify_workerpool(args: SigverifyArgs) -> Vec<WorkerHandle> {
    let SigverifyArgs {
        num_workers,
        admin_keys,
        rx,
        sequencer_tx,
        shutdown_token,
    } = args;
    let mut handles = Vec::with_capacity(num_workers);
    let admin_keys = Arc::new(admin_keys);

    for worker_id in 0..num_workers {
        let rx = rx.clone();
        let tx = sequencer_tx.clone();
        let shutdown = shutdown_token.clone();
        let admin_keys = admin_keys.clone();

        let handle = tokio::spawn(async move {
            info!("Sigverify worker {} started", worker_id);

            loop {
                tokio::select! {
                    // Process transactions
                    result = rx.recv() => {
                        match result {
                            Ok(Some(transaction)) => {
                                let result = sigverify_transaction(&transaction, &admin_keys).await;
                                match result {
                                    SigverifyResult::Valid(_) => {
                                        // Send to sequencer (unbounded, no await needed)
                                        match tx.send(transaction) {
                                            Ok(_) => {
                                                debug!("Worker {} sent transaction to sequencer", worker_id);
                                            }
                                            Err(_) => {
                                                warn!(
                                                    "Worker {} failed to send to sequencer - channel closed",
                                                    worker_id
                                                );
                                                break;
                                            }
                                        }
                                    }
                                    SigverifyResult::InvalidTransaction(transaction_type) => {
                                        warn!(
                                            "Worker {} rejected invalid transaction {}: {:?}",
                                            worker_id,
                                            transaction.signature(),
                                            transaction_type.to_string()
                                        );
                                        continue;
                                    }
                                    SigverifyResult::NotSignedByAdmin => {
                                        warn!(
                                            "Worker {} rejected admin transaction not signed by admin: {}",
                                            worker_id,
                                            transaction.signature()
                                        );
                                        continue;
                                    }
                                    SigverifyResult::SigverifyFailed(e) => {
                                        warn!("Worker {} sigverify failed: {}", worker_id, e);
                                        continue;
                                    }
                                }
                            }
                            Ok(None) | Err(_) => {
                                debug!("Worker {} channel closed", worker_id);
                                break;
                            }
                        }
                    }

                    // Handle shutdown signal
                    _ = shutdown.cancelled() => {
                        debug!("Worker {} received shutdown signal", worker_id);
                        break;
                    }
                }
            }

            info!("Sigverify worker {} stopped", worker_id);
        });

        handles.push(WorkerHandle::new(
            format!("Sigverify-{}", worker_id),
            handle,
        ));
    }
    handles
}
