use crate::channel_utils::send_guaranteed;
use crate::error::{OperatorError, ProgramError};
use crate::operator::instruction_util::{
    mint_idempotency_memo, MintToBuilder, TransactionBuilder, WithdrawalRemintInfo,
};
use crate::operator::utils::mint_util::MintCache;
use crate::operator::{
    find_allowed_mint_pda, find_event_authority_pda, find_operator_pda,
    tree_constants::MAX_TREE_LEAVES, MintToBuilderWithTxnId, ReleaseFundsBuilderWithNonce,
    SignerUtil,
};
use crate::storage::common::models::DbTransaction;
use crate::storage::Storage;
use crate::ProgramType;
use contra_escrow_program_client::instructions::{ReleaseFundsBuilder, ResetSmtRootBuilder};
use solana_sdk::pubkey::Pubkey;
use spl_associated_token_account::get_associated_token_address_with_program_id;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, info, info_span, Instrument};

pub struct ProcessorState {
    pub admin_pubkey: Pubkey,
    pub release_funds_state: Option<ReleaseFundsState>,
    pub mint_cache: MintCache,
}

pub struct ReleaseFundsState {
    pub instance_pda: Pubkey,
    pub operator_pubkey: Pubkey,
    pub operator_pda: Pubkey,
    pub event_authority_pda: Pubkey,
    pub allowed_mints: HashMap<String, Pubkey>,
    pub instance_atas: HashMap<String, Pubkey>,
}

impl ProcessorState {
    pub fn new_with_release_funds_state(
        instance_pda: Pubkey,
        storage: Arc<Storage>,
        rpc_client: Arc<crate::operator::RpcClientWithRetry>,
    ) -> Self {
        let operator_pubkey = SignerUtil::get_operator_pubkey();
        let operator_pda = find_operator_pda(&instance_pda, &operator_pubkey);

        let event_authority_pda = find_event_authority_pda();

        Self {
            admin_pubkey: SignerUtil::get_admin_pubkey(),
            release_funds_state: Some(ReleaseFundsState {
                instance_pda,
                operator_pubkey,
                operator_pda,
                event_authority_pda,
                allowed_mints: HashMap::new(),
                instance_atas: HashMap::new(),
            }),
            mint_cache: MintCache::with_rpc(storage, rpc_client),
        }
    }

    pub fn new_with_storage(
        storage: Arc<Storage>,
        mint_rpc_client: Arc<crate::operator::RpcClientWithRetry>,
    ) -> Self {
        Self {
            admin_pubkey: SignerUtil::get_admin_pubkey(),
            release_funds_state: None,
            mint_cache: MintCache::with_rpc(storage, mint_rpc_client),
        }
    }
}

impl ReleaseFundsState {
    pub fn get_allowed_mint_pda(&mut self, mint: &Pubkey) -> Pubkey {
        *self
            .allowed_mints
            .entry(mint.to_string())
            .or_insert_with(|| find_allowed_mint_pda(&self.instance_pda, mint))
    }

    pub fn get_instance_ata(&mut self, mint: &Pubkey, token_program: &Pubkey) -> Pubkey {
        *self
            .instance_atas
            .entry(mint.to_string())
            .or_insert_with(|| {
                get_associated_token_address_with_program_id(
                    &self.instance_pda,
                    mint,
                    token_program,
                )
            })
    }
}

/// Processes and validates transactions before sending to blockchain
///
/// Receives transactions from fetcher, validates them, and forwards to sender
pub async fn run_processor(
    fetcher_rx: mpsc::Receiver<DbTransaction>,
    sender_tx: mpsc::Sender<TransactionBuilder>,
    program_type: ProgramType,
    instance_pda: Option<Pubkey>,
    storage: Arc<Storage>,
    rpc_client: Arc<crate::operator::RpcClientWithRetry>,
    source_rpc_client: Option<Arc<crate::operator::RpcClientWithRetry>>,
) {
    info!("Starting processor");

    match program_type {
        ProgramType::Withdraw => {
            let mut processor_state = ProcessorState::new_with_release_funds_state(
                instance_pda.expect("Missing instance PDA"),
                storage,
                rpc_client,
            );

            if let Err(e) = process_release_funds(&mut processor_state, fetcher_rx, sender_tx).await
            {
                tracing::error!("Process release funds error: {}", e);
            }
        }
        ProgramType::Escrow => {
            // Use source_rpc_client for mint cache if available, otherwise fall back to rpc_client
            let mint_rpc_client = source_rpc_client.unwrap_or_else(|| rpc_client.clone());
            let mut processor_state = ProcessorState::new_with_storage(storage, mint_rpc_client);

            if let Err(e) = process_deposit_funds(&mut processor_state, fetcher_rx, sender_tx).await
            {
                tracing::error!("Deposit funds error: {}", e);
            }
        }
    }
}

pub async fn process_release_funds(
    processor_state: &mut ProcessorState,
    mut fetcher_rx: mpsc::Receiver<DbTransaction>,
    sender_tx: mpsc::Sender<TransactionBuilder>,
) -> Result<(), OperatorError> {
    let release_funds_state = processor_state
        .release_funds_state
        .as_mut()
        .ok_or(OperatorError::MissingBuilder)?;

    while let Some(transaction) = fetcher_rx.recv().await {
        let span = info_span!("process", trace_id = %transaction.trace_id, txn_id = transaction.id);

        async {
            let nonce = transaction
                .withdrawal_nonce
                .expect("withdrawal transaction must have withdrawal_nonce")
                as u64;

            // Check if we need to rotate the tree before processing this transaction
            if nonce > 0 && nonce.is_multiple_of(MAX_TREE_LEAVES as u64) {
                info!("Tree rotation boundary detected at nonce {}", nonce);

                // Send ResetSmtRoot transaction BEFORE the boundary nonce
                let mut rotation_builder = ResetSmtRootBuilder::new();
                rotation_builder
                    .payer(processor_state.admin_pubkey)
                    .operator(release_funds_state.operator_pubkey)
                    .instance(release_funds_state.instance_pda)
                    .operator_pda(release_funds_state.operator_pda)
                    .event_authority(release_funds_state.event_authority_pda);

                let rotation_tx = TransactionBuilder::ResetSmtRoot(Box::new(rotation_builder));

                send_guaranteed(&sender_tx, rotation_tx, "reset smt root")
                    .await
                    .map_err(OperatorError::ChannelSend)?;

                info!("Sent ResetSmtRoot transaction for tree rotation");
            }

            let mut builder = ReleaseFundsBuilder::new();

            let mint =
                Pubkey::from_str(&transaction.mint).map_err(|e| OperatorError::InvalidPubkey {
                    pubkey: transaction.mint.clone(),
                    reason: e.to_string(),
                })?;
            let recipient = Pubkey::from_str(&transaction.recipient).map_err(|e| {
                OperatorError::InvalidPubkey {
                    pubkey: transaction.recipient.clone(),
                    reason: e.to_string(),
                }
            })?;

            // Fetch mint metadata from cache (or storage if not cached)
            let mint_metadata = processor_state.mint_cache.get_mint_metadata(&mint).await?;
            let token_program = mint_metadata.token_program;

            let allowed_mint_pda = release_funds_state.get_allowed_mint_pda(&mint);
            let instance_ata = release_funds_state.get_instance_ata(&mint, &token_program);

            let recipient_ata =
                get_associated_token_address_with_program_id(&recipient, &mint, &token_program);

            // Sibling proofs and  New withdrawal root not set, will be set by sender
            builder
                .payer(processor_state.admin_pubkey)
                .operator(release_funds_state.operator_pubkey)
                .instance(release_funds_state.instance_pda)
                .operator_pda(release_funds_state.operator_pda)
                .mint(mint)
                .allowed_mint(allowed_mint_pda)
                .user_ata(recipient_ata)
                .instance_ata(instance_ata)
                .token_program(token_program)
                .user(recipient)
                .transaction_nonce(nonce);

            let amount = u64::try_from(transaction.amount).map_err(|_| {
                OperatorError::Program(ProgramError::InvalidBuilder {
                    reason: format!(
                        "negative withdrawal amount {} for transaction {}",
                        transaction.amount, transaction.id
                    ),
                })
            })?;
            builder.amount(amount);

            // Build remint info for token recovery on permanent failure.
            // Uses Contra token program (not mainnet) since remint happens on Contra.
            let contra_token_program = processor_state.mint_cache.get_contra_token_program();
            let remint_user_ata = get_associated_token_address_with_program_id(
                &recipient,
                &mint,
                &contra_token_program,
            );
            let remint_info = WithdrawalRemintInfo {
                transaction_id: transaction.id,
                trace_id: transaction.trace_id.clone(),
                mint,
                user: recipient,
                user_ata: remint_user_ata,
                token_program: contra_token_program,
                amount,
            };

            info!("Processing withdrawal");

            let wrapped =
                TransactionBuilder::ReleaseFunds(Box::new(ReleaseFundsBuilderWithNonce {
                    builder,
                    nonce,
                    transaction_id: transaction.id,
                    trace_id: transaction.trace_id.clone(),
                    remint_info: Some(remint_info),
                }));

            send_guaranteed(&sender_tx, wrapped, "processed release funds")
                .await
                .map_err(OperatorError::ChannelSend)?;

            Ok::<(), OperatorError>(())
        }
        .instrument(span)
        .await?;
    }

    Ok(())
}

pub async fn process_deposit_funds(
    processor_state: &mut ProcessorState,
    mut fetcher_rx: mpsc::Receiver<DbTransaction>,
    sender_tx: mpsc::Sender<TransactionBuilder>,
) -> Result<(), OperatorError> {
    while let Some(transaction) = fetcher_rx.recv().await {
        let span = info_span!("process", trace_id = %transaction.trace_id, txn_id = transaction.id);

        async {
            let proc_t0 = tokio::time::Instant::now();
            let mint =
                Pubkey::from_str(&transaction.mint).map_err(|e| OperatorError::InvalidPubkey {
                    pubkey: transaction.mint.clone(),
                    reason: e.to_string(),
                })?;
            let recipient = Pubkey::from_str(&transaction.recipient).map_err(|e| {
                OperatorError::InvalidPubkey {
                    pubkey: transaction.recipient.clone(),
                    reason: e.to_string(),
                }
            })?;

            let token_program = processor_state.mint_cache.get_contra_token_program();

            let recipient_ata =
                get_associated_token_address_with_program_id(&recipient, &mint, &token_program);

            let mut builder = MintToBuilder::new();
            builder
                .mint(mint)
                .recipient(recipient)
                .recipient_ata(recipient_ata)
                .payer(processor_state.admin_pubkey)
                .mint_authority(processor_state.admin_pubkey)
                .token_program(token_program)
                .amount(transaction.amount as u64)
                .idempotency_memo(mint_idempotency_memo(transaction.id));

            let proc_elapsed_ms = proc_t0.elapsed().as_millis();
            info!(proc_elapsed_ms, "Processing deposit");

            let wrapped = TransactionBuilder::Mint(Box::new(MintToBuilderWithTxnId {
                builder,
                txn_id: transaction.id,
                trace_id: transaction.trace_id.clone(),
            }));

            let send_t0 = tokio::time::Instant::now();
            send_guaranteed(&sender_tx, wrapped, "processed deposit")
                .await
                .map_err(OperatorError::ChannelSend)?;
            let send_elapsed_ms = send_t0.elapsed().as_millis();
            // Any wait >1ms means the sender channel is full — sender is the bottleneck.
            if send_elapsed_ms > 1 {
                debug!(
                    send_elapsed_ms,
                    sender_capacity = sender_tx.capacity(),
                    "Processor blocked sending to sender (sender back-pressure)"
                );
            }

            Ok::<(), OperatorError>(())
        }
        .instrument(span)
        .await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operator::find_allowed_mint_pda;
    use crate::storage::common::storage::mock::MockStorage;

    fn make_release_funds_state() -> ReleaseFundsState {
        ReleaseFundsState {
            instance_pda: Pubkey::new_unique(),
            operator_pubkey: Pubkey::new_unique(),
            operator_pda: Pubkey::new_unique(),
            event_authority_pda: Pubkey::new_unique(),
            allowed_mints: HashMap::new(),
            instance_atas: HashMap::new(),
        }
    }

    #[test]
    fn get_allowed_mint_pda_derives_and_caches() {
        let mut state = make_release_funds_state();
        let mint = Pubkey::new_unique();

        let pda1 = state.get_allowed_mint_pda(&mint);
        let pda2 = state.get_allowed_mint_pda(&mint);

        assert_eq!(pda1, pda2);
        assert_eq!(pda1, find_allowed_mint_pda(&state.instance_pda, &mint));
        assert_eq!(state.allowed_mints.len(), 1);
    }

    #[test]
    fn get_allowed_mint_pda_different_mints() {
        let mut state = make_release_funds_state();
        let mint_a = Pubkey::new_unique();
        let mint_b = Pubkey::new_unique();

        assert_ne!(
            state.get_allowed_mint_pda(&mint_a),
            state.get_allowed_mint_pda(&mint_b)
        );
        assert_eq!(state.allowed_mints.len(), 2);
    }

    #[test]
    fn get_instance_ata_derives_and_caches() {
        let mut state = make_release_funds_state();
        let mint = Pubkey::new_unique();
        let tp = spl_token::id();

        let ata1 = state.get_instance_ata(&mint, &tp);
        let ata2 = state.get_instance_ata(&mint, &tp);

        assert_eq!(ata1, ata2);
        let expected =
            get_associated_token_address_with_program_id(&state.instance_pda, &mint, &tp);
        assert_eq!(ata1, expected);
        assert_eq!(state.instance_atas.len(), 1);
    }

    #[test]
    fn get_instance_ata_different_mints() {
        let mut state = make_release_funds_state();
        let mint_a = Pubkey::new_unique();
        let mint_b = Pubkey::new_unique();
        let tp = spl_token::id();

        assert_ne!(
            state.get_instance_ata(&mint_a, &tp),
            state.get_instance_ata(&mint_b, &tp)
        );
        assert_eq!(state.instance_atas.len(), 2);
    }

    #[tokio::test]
    async fn process_release_funds_missing_state_errors() {
        let mock = MockStorage::new();
        let storage = Arc::new(Storage::Mock(mock));
        let mut ps = ProcessorState {
            admin_pubkey: Pubkey::new_unique(),
            release_funds_state: None,
            mint_cache: crate::operator::MintCache::new(storage),
        };
        // Keep tx alive so channel isn't closed — error must come from missing state
        let (_tx, rx) = mpsc::channel::<DbTransaction>(1);
        let (sender_tx, _sender_rx) = mpsc::channel(1);

        let result = process_release_funds(&mut ps, rx, sender_tx).await;
        assert!(
            matches!(result, Err(crate::error::OperatorError::MissingBuilder)),
            "expected MissingBuilder, got: {:?}",
            result
        );
    }

    /// A valid withdrawal transaction is enriched with PDAs and ATA addresses then forwarded
    /// to the sender channel as a ReleaseFunds builder.
    #[tokio::test]
    async fn process_release_funds_sends_transaction_builder() {
        let mock = MockStorage::new();
        let storage = Arc::new(Storage::Mock(mock));
        let mut ps = ProcessorState {
            admin_pubkey: Pubkey::new_unique(),
            release_funds_state: Some(make_release_funds_state()),
            mint_cache: crate::operator::MintCache::new(storage.clone()),
        };

        // Add a mint to the mock storage so mint_cache can find it
        let mint_pubkey = Pubkey::new_unique();
        let recipient = Pubkey::new_unique();
        {
            let mock_storage = match storage.as_ref() {
                Storage::Mock(m) => m,
                _ => unreachable!(),
            };
            mock_storage.mints.lock().unwrap().insert(
                mint_pubkey.to_string(),
                crate::storage::common::models::DbMint {
                    mint_address: mint_pubkey.to_string(),
                    decimals: 6,
                    token_program: spl_token::id().to_string(),
                    created_at: chrono::Utc::now(),
                },
            );
        }

        let (fetcher_tx, fetcher_rx) = mpsc::channel::<DbTransaction>(1);
        let (sender_tx, mut sender_rx) = mpsc::channel(10);

        let txn = DbTransaction {
            id: 1,
            signature: "test_sig".to_string(),
            trace_id: "trace-1".to_string(),
            slot: 100,
            initiator: "initiator".to_string(),
            recipient: recipient.to_string(),
            mint: mint_pubkey.to_string(),
            amount: 1000,
            memo: None,
            transaction_type: crate::storage::common::models::TransactionType::Withdrawal,
            withdrawal_nonce: Some(5),
            status: crate::storage::common::models::TransactionStatus::Processing,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            processed_at: None,
            counterpart_signature: None,
            remint_signatures: None,
            pending_remint_deadline_at: None,
        };

        fetcher_tx.send(txn).await.unwrap();
        drop(fetcher_tx);

        let result = process_release_funds(&mut ps, fetcher_rx, sender_tx).await;
        assert!(result.is_ok());

        let msg = sender_rx.recv().await.unwrap();
        let TransactionBuilder::ReleaseFunds(b) = msg else {
            panic!("expected ReleaseFunds, got a different variant");
        };
        assert_eq!(b.nonce, 5);
        assert_eq!(b.transaction_id, 1);
        assert_eq!(b.trace_id, "trace-1");
    }

    /// When the nonce lands exactly on MAX_TREE_LEAVES, a ResetSmtRoot transaction must be
    /// sent before the ReleaseFunds transaction to rotate the SMT root.
    #[tokio::test]
    async fn process_release_funds_tree_rotation_sends_reset_first() {
        let mock = MockStorage::new();
        let storage = Arc::new(Storage::Mock(mock));
        let mut ps = ProcessorState {
            admin_pubkey: Pubkey::new_unique(),
            release_funds_state: Some(make_release_funds_state()),
            mint_cache: crate::operator::MintCache::new(storage.clone()),
        };

        let mint_pubkey = Pubkey::new_unique();
        let recipient = Pubkey::new_unique();
        {
            let mock_storage = match storage.as_ref() {
                Storage::Mock(m) => m,
                _ => unreachable!(),
            };
            mock_storage.mints.lock().unwrap().insert(
                mint_pubkey.to_string(),
                crate::storage::common::models::DbMint {
                    mint_address: mint_pubkey.to_string(),
                    decimals: 6,
                    token_program: spl_token::id().to_string(),
                    created_at: chrono::Utc::now(),
                },
            );
        }

        let (fetcher_tx, fetcher_rx) = mpsc::channel::<DbTransaction>(1);
        let (sender_tx, mut sender_rx) = mpsc::channel(10);

        // Use nonce at tree rotation boundary (MAX_TREE_LEAVES)
        let txn = DbTransaction {
            id: 1,
            signature: "test_sig".to_string(),
            trace_id: "trace-1".to_string(),
            slot: 100,
            initiator: "initiator".to_string(),
            recipient: recipient.to_string(),
            mint: mint_pubkey.to_string(),
            amount: 1000,
            memo: None,
            transaction_type: crate::storage::common::models::TransactionType::Withdrawal,
            withdrawal_nonce: Some(MAX_TREE_LEAVES as i64),
            status: crate::storage::common::models::TransactionStatus::Processing,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            processed_at: None,
            counterpart_signature: None,
            remint_signatures: None,
            pending_remint_deadline_at: None,
        };

        fetcher_tx.send(txn).await.unwrap();
        drop(fetcher_tx);

        let result = process_release_funds(&mut ps, fetcher_rx, sender_tx).await;
        assert!(result.is_ok());

        // First message must be ResetSmtRoot — rotation happens before the boundary withdrawal
        let msg1 = sender_rx.recv().await.unwrap();
        assert!(
            matches!(msg1, TransactionBuilder::ResetSmtRoot(_)),
            "expected ResetSmtRoot first, got: {:?}",
            std::mem::discriminant(&msg1)
        );

        // Second message must be the ReleaseFunds for the boundary nonce itself
        let msg2 = sender_rx.recv().await.unwrap();
        let TransactionBuilder::ReleaseFunds(b) = msg2 else {
            panic!("expected ReleaseFunds second, got a different variant");
        };
        assert_eq!(b.nonce, MAX_TREE_LEAVES as u64);
        assert_eq!(b.transaction_id, 1);

        // No further messages — exactly two were sent
        assert!(sender_rx.try_recv().is_err(), "unexpected third message");
    }

    /// A mint field that cannot be parsed as a Pubkey must surface as an InvalidPubkey error
    /// rather than panicking or silently skipping the transaction.
    #[tokio::test]
    async fn process_release_funds_invalid_mint_errors() {
        let mock = MockStorage::new();
        let storage = Arc::new(Storage::Mock(mock));
        let mut ps = ProcessorState {
            admin_pubkey: Pubkey::new_unique(),
            release_funds_state: Some(make_release_funds_state()),
            mint_cache: crate::operator::MintCache::new(storage),
        };

        let (fetcher_tx, fetcher_rx) = mpsc::channel::<DbTransaction>(1);
        let (sender_tx, _sender_rx) = mpsc::channel(10);

        let txn = DbTransaction {
            id: 1,
            signature: "test_sig".to_string(),
            trace_id: "trace-1".to_string(),
            slot: 100,
            initiator: "initiator".to_string(),
            recipient: Pubkey::new_unique().to_string(),
            mint: "not_a_valid_pubkey".to_string(),
            amount: 1000,
            memo: None,
            transaction_type: crate::storage::common::models::TransactionType::Withdrawal,
            withdrawal_nonce: Some(1),
            status: crate::storage::common::models::TransactionStatus::Processing,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            processed_at: None,
            counterpart_signature: None,
            remint_signatures: None,
            pending_remint_deadline_at: None,
        };

        fetcher_tx.send(txn).await.unwrap();
        drop(fetcher_tx);

        let err = process_release_funds(&mut ps, fetcher_rx, sender_tx)
            .await
            .unwrap_err();
        assert!(
            matches!(err, OperatorError::InvalidPubkey { ref pubkey, .. } if pubkey == "not_a_valid_pubkey"),
            "expected InvalidPubkey for bad mint, got: {:?}",
            err
        );
    }

    /// A valid deposit transaction is wrapped as a Mint builder with the correct ATA and
    /// idempotency memo, then forwarded to the sender channel.
    #[tokio::test]
    async fn process_deposit_funds_sends_mint_builder() {
        let mock = MockStorage::new();
        let storage = Arc::new(Storage::Mock(mock));
        let mut ps = ProcessorState {
            admin_pubkey: Pubkey::new_unique(),
            release_funds_state: None,
            mint_cache: crate::operator::MintCache::new(storage),
        };

        let mint_pubkey = Pubkey::new_unique();
        let recipient = Pubkey::new_unique();

        let (fetcher_tx, fetcher_rx) = mpsc::channel::<DbTransaction>(1);
        let (sender_tx, mut sender_rx) = mpsc::channel(10);

        let txn = DbTransaction {
            id: 1,
            signature: "test_sig".to_string(),
            trace_id: "trace-1".to_string(),
            slot: 100,
            initiator: "initiator".to_string(),
            recipient: recipient.to_string(),
            mint: mint_pubkey.to_string(),
            amount: 1000,
            memo: None,
            transaction_type: crate::storage::common::models::TransactionType::Deposit,
            withdrawal_nonce: None,
            status: crate::storage::common::models::TransactionStatus::Processing,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            processed_at: None,
            counterpart_signature: None,
            remint_signatures: None,
            pending_remint_deadline_at: None,
        };

        fetcher_tx.send(txn).await.unwrap();
        drop(fetcher_tx);

        let result = process_deposit_funds(&mut ps, fetcher_rx, sender_tx).await;
        assert!(result.is_ok());

        let msg = sender_rx.recv().await.unwrap();
        let TransactionBuilder::Mint(b) = msg else {
            panic!("expected Mint, got a different variant");
        };
        assert_eq!(b.txn_id, 1);
        assert_eq!(b.trace_id, "trace-1");
    }

    /// A non-base58 mint string must fail with InvalidPubkey; the error propagates out of
    /// process_deposit_funds rather than being swallowed.
    #[tokio::test]
    async fn process_deposit_funds_invalid_mint_errors() {
        let mock = MockStorage::new();
        let storage = Arc::new(Storage::Mock(mock));
        let mut ps = ProcessorState {
            admin_pubkey: Pubkey::new_unique(),
            release_funds_state: None,
            mint_cache: crate::operator::MintCache::new(storage),
        };

        let (fetcher_tx, fetcher_rx) = mpsc::channel::<DbTransaction>(1);
        let (sender_tx, _sender_rx) = mpsc::channel(10);

        let txn = DbTransaction {
            id: 1,
            signature: "test_sig".to_string(),
            trace_id: "trace-1".to_string(),
            slot: 100,
            initiator: "initiator".to_string(),
            recipient: Pubkey::new_unique().to_string(),
            mint: "not_a_valid_pubkey".to_string(),
            amount: 1000,
            memo: None,
            transaction_type: crate::storage::common::models::TransactionType::Deposit,
            withdrawal_nonce: None,
            status: crate::storage::common::models::TransactionStatus::Processing,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            processed_at: None,
            counterpart_signature: None,
            remint_signatures: None,
            pending_remint_deadline_at: None,
        };

        fetcher_tx.send(txn).await.unwrap();
        drop(fetcher_tx);

        let err = process_deposit_funds(&mut ps, fetcher_rx, sender_tx)
            .await
            .unwrap_err();
        assert!(
            matches!(err, OperatorError::InvalidPubkey { ref pubkey, .. } if pubkey == "not_a_valid_pubkey"),
            "expected InvalidPubkey for bad mint, got: {:?}",
            err
        );
    }

    /// An already-closed fetcher channel means there are no transactions to process;
    /// the function should return Ok(()) immediately without touching the sender.
    #[tokio::test]
    async fn process_deposit_funds_empty_channel_returns_ok() {
        let mock = MockStorage::new();
        let storage = Arc::new(Storage::Mock(mock));
        let mut ps = ProcessorState {
            admin_pubkey: Pubkey::new_unique(),
            release_funds_state: None,
            mint_cache: crate::operator::MintCache::new(storage),
        };

        let (fetcher_tx, fetcher_rx) = mpsc::channel::<DbTransaction>(1);
        let (sender_tx, mut sender_rx) = mpsc::channel(10);

        drop(fetcher_tx); // close channel immediately — no transactions to process

        process_deposit_funds(&mut ps, fetcher_rx, sender_tx)
            .await
            .unwrap();

        // Nothing was sent; channel is empty and the sender was dropped by the function
        assert!(
            sender_rx.try_recv().is_err(),
            "expected empty sender channel"
        );
    }

    /// A recipient field that is not a valid base58 pubkey must return an error; the ATA
    /// derivation step must never be reached with garbage input.
    #[tokio::test]
    async fn process_deposit_funds_invalid_recipient_errors() {
        let mock = MockStorage::new();
        let storage = Arc::new(Storage::Mock(mock));
        let mut ps = ProcessorState {
            admin_pubkey: Pubkey::new_unique(),
            release_funds_state: None,
            mint_cache: crate::operator::MintCache::new(storage),
        };

        let (fetcher_tx, fetcher_rx) = mpsc::channel::<DbTransaction>(1);
        let (sender_tx, _sender_rx) = mpsc::channel(10);

        let txn = DbTransaction {
            id: 1,
            signature: "test_sig".to_string(),
            trace_id: "trace-1".to_string(),
            slot: 100,
            initiator: "initiator".to_string(),
            recipient: "not_a_valid_pubkey".to_string(), // invalid recipient
            mint: Pubkey::new_unique().to_string(),
            amount: 1000,
            memo: None,
            transaction_type: crate::storage::common::models::TransactionType::Deposit,
            withdrawal_nonce: None,
            status: crate::storage::common::models::TransactionStatus::Processing,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            processed_at: None,
            counterpart_signature: None,
            remint_signatures: None,
            pending_remint_deadline_at: None,
        };

        fetcher_tx.send(txn).await.unwrap();
        drop(fetcher_tx);

        let err = process_deposit_funds(&mut ps, fetcher_rx, sender_tx)
            .await
            .unwrap_err();
        assert!(
            matches!(err, OperatorError::InvalidPubkey { ref pubkey, .. } if pubkey == "not_a_valid_pubkey"),
            "expected InvalidPubkey for bad recipient, got: {:?}",
            err
        );
    }

    /// An unparseable recipient pubkey must fail with InvalidPubkey even when the mint is
    /// valid and the release_funds_state is fully populated.
    #[tokio::test]
    async fn process_release_funds_invalid_recipient_errors() {
        let mock = MockStorage::new();
        let storage = Arc::new(Storage::Mock(mock));
        let mut ps = ProcessorState {
            admin_pubkey: Pubkey::new_unique(),
            release_funds_state: Some(make_release_funds_state()),
            mint_cache: crate::operator::MintCache::new(storage.clone()),
        };

        let mint_pubkey = Pubkey::new_unique();
        {
            let mock_storage = match storage.as_ref() {
                Storage::Mock(m) => m,
                _ => unreachable!(),
            };
            mock_storage.mints.lock().unwrap().insert(
                mint_pubkey.to_string(),
                crate::storage::common::models::DbMint {
                    mint_address: mint_pubkey.to_string(),
                    decimals: 6,
                    token_program: spl_token::id().to_string(),
                    created_at: chrono::Utc::now(),
                },
            );
        }

        let (fetcher_tx, fetcher_rx) = mpsc::channel::<DbTransaction>(1);
        let (sender_tx, _sender_rx) = mpsc::channel(10);

        let txn = DbTransaction {
            id: 1,
            signature: "test_sig".to_string(),
            trace_id: "trace-1".to_string(),
            slot: 100,
            initiator: "initiator".to_string(),
            recipient: "not_a_valid_pubkey".to_string(), // invalid recipient
            mint: mint_pubkey.to_string(),
            amount: 1000,
            memo: None,
            transaction_type: crate::storage::common::models::TransactionType::Withdrawal,
            withdrawal_nonce: Some(5),
            status: crate::storage::common::models::TransactionStatus::Processing,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            processed_at: None,
            counterpart_signature: None,
            remint_signatures: None,
            pending_remint_deadline_at: None,
        };

        fetcher_tx.send(txn).await.unwrap();
        drop(fetcher_tx);

        let err = process_release_funds(&mut ps, fetcher_rx, sender_tx)
            .await
            .unwrap_err();
        assert!(
            matches!(err, OperatorError::InvalidPubkey { ref pubkey, .. } if pubkey == "not_a_valid_pubkey"),
            "expected InvalidPubkey for bad recipient, got: {:?}",
            err
        );
    }
}
