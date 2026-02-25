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

#[cfg(test)]
mod tests {
    use super::*;
    use solana_sdk::{
        hash::Hash,
        instruction::{AccountMeta, Instruction},
        signature::{Keypair, Signature, Signer},
        transaction::{SanitizedTransaction, Transaction},
    };
    use std::collections::HashSet;

    /// Build a signed `SanitizedTransaction` from instructions + signers.
    fn sanitize(
        instructions: &[Instruction],
        payer: &Keypair,
        signers: &[&Keypair],
    ) -> SanitizedTransaction {
        let tx = Transaction::new_signed_with_payer(
            instructions,
            Some(&payer.pubkey()),
            signers,
            Hash::default(),
        );
        SanitizedTransaction::try_from_legacy_transaction(tx, &HashSet::new()).unwrap()
    }

    fn spl_transfer_ix(from_ata: &Pubkey, to_ata: &Pubkey, authority: &Pubkey) -> Instruction {
        spl_token::instruction::transfer(&spl_token::id(), from_ata, to_ata, authority, &[], 1_000)
            .unwrap()
    }

    fn initialize_mint_ix(mint: &Pubkey, authority: &Pubkey) -> Instruction {
        spl_token::instruction::initialize_mint(&spl_token::id(), mint, authority, None, 6).unwrap()
    }

    // --- C9: empty transaction (no instructions) must be rejected ------

    #[tokio::test]
    async fn empty_transaction_rejected() {
        let payer = Keypair::new();
        let tx = sanitize(&[], &payer, &[&payer]);
        let result = sigverify_transaction(&tx, &[]).await;
        assert!(
            matches!(
                result,
                SigverifyResult::InvalidTransaction(TransactionType::Empty)
            ),
            "expected InvalidTransaction(Empty), got {result}"
        );
    }

    // --- C8: mixed admin + non-admin instructions must be rejected -----

    #[tokio::test]
    async fn mixed_transaction_rejected() {
        let admin = Keypair::new();
        let user = Keypair::new();
        let mint = Pubkey::new_unique();
        let from_ata = Pubkey::new_unique();
        let to_ata = Pubkey::new_unique();

        let admin_ix = initialize_mint_ix(&mint, &admin.pubkey());
        let normal_ix = spl_transfer_ix(&from_ata, &to_ata, &user.pubkey());

        let tx = sanitize(&[admin_ix, normal_ix], &admin, &[&admin, &user]);
        let result = sigverify_transaction(&tx, &[admin.pubkey()]).await;
        assert!(
            matches!(
                result,
                SigverifyResult::InvalidTransaction(TransactionType::Mixed)
            ),
            "expected InvalidTransaction(Mixed), got {result}"
        );
    }

    // --- C7: admin instruction without admin signer must be rejected ---

    #[tokio::test]
    async fn admin_instruction_without_admin_signer_rejected() {
        let non_admin = Keypair::new();
        let mint = Pubkey::new_unique();
        let real_admin = Pubkey::new_unique(); // in admin_keys but not a tx signer

        let ix = initialize_mint_ix(&mint, &non_admin.pubkey());
        let tx = sanitize(&[ix], &non_admin, &[&non_admin]);
        let result = sigverify_transaction(&tx, &[real_admin]).await;
        assert!(
            matches!(result, SigverifyResult::NotSignedByAdmin),
            "expected NotSignedByAdmin, got {result}"
        );
    }

    #[tokio::test]
    async fn admin_instruction_with_admin_signer_accepted() {
        let admin = Keypair::new();
        let mint = Pubkey::new_unique();

        let ix = initialize_mint_ix(&mint, &admin.pubkey());
        let tx = sanitize(&[ix], &admin, &[&admin]);
        let result = sigverify_transaction(&tx, &[admin.pubkey()]).await;
        assert!(
            matches!(result, SigverifyResult::Valid(TransactionType::Admin)),
            "expected Valid(Admin), got {result}"
        );
    }

    // --- C5: tampered signature must be rejected -------------------------

    #[tokio::test]
    async fn tampered_signature_rejected() {
        let payer = Keypair::new();
        let from_ata = Pubkey::new_unique();
        let to_ata = Pubkey::new_unique();
        let ix = spl_transfer_ix(&from_ata, &to_ata, &payer.pubkey());

        // Build a properly signed transaction, then replace the signature
        let mut tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer.pubkey()),
            &[&payer],
            Hash::default(),
        );
        // Replace signature with a corrupted copy
        let mut sig_bytes = <[u8; 64]>::from(tx.signatures[0]);
        sig_bytes[0] ^= 0xff;
        tx.signatures[0] = Signature::from(sig_bytes);

        let sanitized =
            SanitizedTransaction::try_from_legacy_transaction(tx, &HashSet::new()).unwrap();
        let result = sigverify_transaction(&sanitized, &[]).await;
        assert!(
            matches!(result, SigverifyResult::SigverifyFailed(_)),
            "expected SigverifyFailed, got {result}"
        );
    }

    // --- Normal happy path: properly signed normal tx accepted ---------

    #[tokio::test]
    async fn valid_normal_transaction_accepted() {
        let payer = Keypair::new();
        let from_ata = Pubkey::new_unique();
        let to_ata = Pubkey::new_unique();
        let ix = spl_transfer_ix(&from_ata, &to_ata, &payer.pubkey());

        let tx = sanitize(&[ix], &payer, &[&payer]);
        let result = sigverify_transaction(&tx, &[]).await;
        assert!(
            matches!(result, SigverifyResult::Valid(TransactionType::Normal)),
            "expected Valid(Normal), got {result}"
        );
    }

    // --- classify_transaction edge cases --------------------------------

    #[tokio::test]
    async fn instruction_with_empty_data_not_counted() {
        // An instruction with no data bytes is skipped by classify_transaction.
        // A tx with only such instructions is classified Empty.
        let payer = Keypair::new();
        let program_id = Pubkey::new_unique();
        let ix = Instruction {
            program_id,
            accounts: vec![AccountMeta::new_readonly(payer.pubkey(), false)],
            data: vec![], // empty data
        };
        let tx = sanitize(&[ix], &payer, &[&payer]);
        let result = sigverify_transaction(&tx, &[]).await;
        assert!(
            matches!(
                result,
                SigverifyResult::InvalidTransaction(TransactionType::Empty)
            ),
            "expected InvalidTransaction(Empty) for empty-data ix, got {result}"
        );
    }
}
