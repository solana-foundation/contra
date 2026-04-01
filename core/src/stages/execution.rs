use {
    crate::{
        accounts::{bob::BOB, AccountsDB},
        nodes::node::WorkerHandle,
        processor::{
            create_transaction_batch_processor, get_transaction_check_results, ContraForkGraph,
        },
        scheduler::ConflictFreeBatch,
        stage_metrics::SharedMetrics,
        stages::AccountSettlement,
        transactions::is_admin_instruction,
        vm::{
            admin::AdminVm, gasless_callback::GaslessCallback,
            gasless_rent_collector::GaslessRentCollector,
        },
    },
    solana_compute_budget::compute_budget::SVMTransactionExecutionBudget,
    solana_sdk::{hash::Hash, pubkey::Pubkey, transaction::SanitizedTransaction},
    solana_svm::transaction_processor::{
        LoadAndExecuteSanitizedTransactionsOutput, TransactionBatchProcessor,
        TransactionProcessingConfig, TransactionProcessingEnvironment,
    },
    solana_svm_feature_set::SVMFeatureSet,
    solana_svm_transaction::svm_message::SVMMessage,
    std::{
        collections::HashSet,
        sync::{Arc, RwLock},
    },
    tokio::sync::mpsc,
    tokio_util::sync::CancellationToken,
    tracing::{debug, error, info},
};

pub struct ExecutionArgs {
    pub batch_rx: mpsc::UnboundedReceiver<ConflictFreeBatch>,
    pub settled_accounts_rx: mpsc::UnboundedReceiver<Vec<(Pubkey, AccountSettlement)>>,
    pub execution_results_tx: mpsc::UnboundedSender<(
        LoadAndExecuteSanitizedTransactionsOutput,
        Vec<SanitizedTransaction>,
    )>,
    pub accountsdb_connection_url: String,
    pub shutdown_token: CancellationToken,
    pub metrics: SharedMetrics,
}

pub struct ExecutionDeps {
    pub bob: BOB,
    pub vm: TransactionBatchProcessor<ContraForkGraph>,
    pub admin_vm: AdminVm,

    // Must prevent this from being dropped
    _fork_graph: Arc<RwLock<ContraForkGraph>>,
}

pub struct ExecutionResult {
    pub admin_transactions: Vec<SanitizedTransaction>,
    pub regular_transactions: Vec<SanitizedTransaction>,
    pub admin_results: Option<LoadAndExecuteSanitizedTransactionsOutput>,
    pub regular_results: Option<LoadAndExecuteSanitizedTransactionsOutput>,
}

pub async fn start_execution_worker(args: ExecutionArgs) -> WorkerHandle {
    let ExecutionArgs {
        mut batch_rx,
        settled_accounts_rx,
        execution_results_tx,
        accountsdb_connection_url,
        shutdown_token,
        metrics,
    } = args;
    let handle = tokio::spawn(async move {
        info!("Execution worker started");

        let accounts_db = AccountsDB::new(&accountsdb_connection_url, true)
            .await
            .unwrap();
        let mut execution_deps = get_execution_deps(accounts_db, settled_accounts_rx).await;

        let mut total_transactions_executed = 0u64;
        let mut total_batches_processed = 0u64;

        loop {
            tokio::select! {
                // Process batches
                result = batch_rx.recv() => {
                    match result {
                        Some(batch) => {
                            let batch_size = batch.transactions.len();
                            debug!("Executor received batch with {} transactions", batch_size);

                            let execution_result = execute_batch(
                                batch,
                                &mut execution_deps,
                            ).await;

                            let num_transactions_executed = execution_result.admin_transactions.len() + execution_result.regular_transactions.len();
                            if !execution_result.admin_transactions.is_empty() {
                                if let Some(admin_results) = execution_result.admin_results {
                                    let len = execution_result.admin_transactions.len();
                                    if let Err(e) = execution_results_tx.send((admin_results, execution_result.admin_transactions)) {
                                        metrics.executor_results_send_failed("admin");
                                        error!("Failed to send admin results: {:?}", e);
                                        break;
                                    }
                                    metrics.executor_results_sent(len);
                                } else {
                                    metrics.executor_missing_results("admin");
                                    error!("Unexpected error: No result found for admin transactions");
                                    break;
                                }
                            }
                            if !execution_result.regular_transactions.is_empty() {
                                if let Some(regular_results) = execution_result.regular_results {
                                    let len = execution_result.regular_transactions.len();
                                    if let Err(e) = execution_results_tx.send((regular_results, execution_result.regular_transactions)) {
                                        metrics.executor_results_send_failed("regular");
                                        error!("Failed to send regular results: {:?}", e);
                                        break;
                                    }
                                    metrics.executor_results_sent(len);
                                } else {
                                    metrics.executor_missing_results("regular");
                                    error!("Unexpected error: No result found for regular transactions");
                                    break;
                                }
                            }

                            total_transactions_executed += num_transactions_executed as u64;
                            total_batches_processed += 1;

                            if total_batches_processed.is_multiple_of(100) {
                                info!("Executor has processed {} batches, {} total transactions",
                                      total_batches_processed, total_transactions_executed);
                            }
                        }
                        None => {
                            info!("Executor stopped - channel closed, executed {} total transactions in {} batches",
                                  total_transactions_executed, total_batches_processed);
                            return;
                        }
                    }
                }

                // Handle shutdown signal
                _ = shutdown_token.cancelled() => {
                    info!("Executor received shutdown signal, executed {} total transactions in {} batches",
                          total_transactions_executed, total_batches_processed);
                    return;
                }
            }
        }
    });

    WorkerHandle::new("Execution".to_string(), handle)
}

pub async fn get_execution_deps(
    accounts_db: AccountsDB,
    settled_accounts_rx: mpsc::UnboundedReceiver<Vec<(Pubkey, AccountSettlement)>>,
) -> ExecutionDeps {
    let bob = BOB::new(accounts_db, settled_accounts_rx).await;
    let feature_set = SVMFeatureSet::all_enabled();
    let compute_budget = SVMTransactionExecutionBudget::default();
    let (vm, _fork_graph) =
        create_transaction_batch_processor(&bob, &feature_set, &compute_budget).unwrap();
    let admin_vm = AdminVm::default();
    ExecutionDeps {
        bob,
        vm,
        admin_vm,
        _fork_graph,
    }
}

pub async fn execute_batch(
    batch: ConflictFreeBatch,
    execution_deps: &mut ExecutionDeps,
) -> ExecutionResult {
    let batch_size = batch.transactions.len();
    debug!("Executing batch with {} transactions", batch_size);

    // Extract all transactions from the batch
    let all_transactions: Vec<_> = batch
        .transactions
        .into_iter()
        .map(|tx| tx.transaction.as_ref().clone())
        .collect();

    // TODO: ConflictFree scheduling should do the admin/non-admin/ATA partitioning
    // This would allow better parallelization and cleaner separation of concerns
    // The scheduler could create separate batches for admin vs regular vs ATA transactions

    // Partition transactions into three categories
    let mut admin_transactions = Vec::new();
    let mut regular_transactions = Vec::new();
    let mut fee_payers = HashSet::new();
    let mut accounts_to_preload = HashSet::new();

    for tx in all_transactions {
        // Collect fee payer BEFORE moving tx
        fee_payers.insert(*tx.fee_payer());
        // Collect all accounts referenced in the transaction
        // This includes program accounts, instruction accounts, and fee payer
        for account in tx.message().account_keys().iter() {
            accounts_to_preload.insert(*account);
        }

        if tx
            .message()
            .program_instructions_iter()
            .any(|(program_id, instruction)| {
                program_id == &spl_token::id()
                    && instruction
                        .data
                        .first()
                        .is_some_and(|t| is_admin_instruction(program_id, *t))
            })
        {
            // Admin SPL transactions
            admin_transactions.push(tx);
        } else {
            // Regular transactions
            regular_transactions.push(tx);
        }
    }

    let num_admin_transactions = admin_transactions.len();
    let num_regular_transactions = regular_transactions.len();
    info!(
        "Batch contains {} admin, and {} regular transactions",
        num_admin_transactions, num_regular_transactions
    );

    // Preload accounts
    let accounts_to_preload = accounts_to_preload.into_iter().collect::<Vec<_>>();
    execution_deps
        .bob
        .preload_accounts(&accounts_to_preload)
        .await;

    // Create processing environment and config
    let feature_set: SVMFeatureSet = SVMFeatureSet::all_enabled();
    // TODO: Use non-default blockhash for TransactionProcessingEnvironment
    // This would add replay attack prevention by ensuring each batch has a unique blockhash
    // Could use a combination of slot number, batch index, or timestamp to generate unique hashes

    // For gasless operation, use our custom gasless rent collector
    let gasless_rent_collector = GaslessRentCollector::new();
    let rent_collector = Some(
        &gasless_rent_collector
            as &dyn solana_svm_rent_collector::svm_rent_collector::SVMRentCollector,
    );

    let processing_environment = TransactionProcessingEnvironment {
        blockhash: Hash::default(), // TODO: Replace with proper blockhash for replay protection
        blockhash_lamports_per_signature: 0, // Gasless - no lamports per signature
        feature_set,
        rent_collector,
        ..Default::default()
    };

    let processing_config = TransactionProcessingConfig {
        ..Default::default()
    };

    // Settle admin transactions immediately so regular transactions see the updates
    let admin_results = if !admin_transactions.is_empty() {
        let admin_results = {
            execution_deps
                .admin_vm
                .load_and_execute_sanitized_transactions(
                    &execution_deps.bob,
                    admin_transactions.as_slice(),
                    get_transaction_check_results(admin_transactions.len()),
                    &processing_environment,
                    &processing_config,
                )
        };

        // Update BOB's in-memory accounts with the execution results
        execution_deps
            .bob
            .update_accounts(&admin_results, &admin_transactions);
        Some(admin_results)
    } else {
        None
    };

    // Now execute regular transactions with updated state

    // Settle regular transactions
    let regular_results = if !regular_transactions.is_empty() {
        let regular_results = {
            // Maybe just move this to the bob
            let gasless_callback = GaslessCallback::new(&execution_deps.bob, fee_payers);
            execution_deps.vm.load_and_execute_sanitized_transactions(
                &gasless_callback,
                regular_transactions.as_slice(),
                get_transaction_check_results(regular_transactions.len()),
                &processing_environment,
                &processing_config,
            )
        };

        // Update BOB's in-memory accounts with the execution results
        execution_deps
            .bob
            .update_accounts(&regular_results, &regular_transactions);
        Some(regular_results)
    } else {
        None
    };

    ExecutionResult {
        admin_transactions,
        regular_transactions,
        admin_results,
        regular_results,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{stage_metrics::NoopMetrics, test_helpers::start_test_postgres};
    use solana_sdk::{
        hash::Hash,
        message::Message,
        pubkey::Pubkey,
        signature::{Keypair, Signer},
        transaction::Transaction,
    };
    use solana_svm::transaction_processor::LoadAndExecuteSanitizedTransactionsOutput;
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    fn create_test_transaction() -> SanitizedTransaction {
        let payer = Keypair::new();
        let instruction = solana_system_interface::instruction::transfer(
            &payer.pubkey(),
            &Pubkey::new_unique(),
            100,
        );
        let message = Message::new(&[instruction], Some(&payer.pubkey()));
        let tx = Transaction::new(&[&payer], message, Hash::default());
        SanitizedTransaction::try_from_legacy_transaction(tx, &HashSet::new())
            .expect("failed to create test transaction")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_execute_batch_empty_batch() {
        let (accounts_db, _pg) = start_test_postgres().await;
        let (_tx, rx) = mpsc::unbounded_channel();
        let mut deps = get_execution_deps(accounts_db, rx).await;

        let empty_batch = ConflictFreeBatch {
            transactions: vec![],
        };

        let result = execute_batch(empty_batch, &mut deps).await;
        assert!(result.admin_transactions.is_empty());
        assert!(result.regular_transactions.is_empty());
        assert!(result.admin_results.is_none());
        assert!(result.regular_results.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_execute_batch_single_normal_transaction() {
        let (accounts_db, _pg) = start_test_postgres().await;
        let (_tx, rx) = mpsc::unbounded_channel();
        let mut deps = get_execution_deps(accounts_db, rx).await;

        let tx = create_test_transaction();
        let batch = ConflictFreeBatch {
            transactions: vec![crate::scheduler::TransactionWithIndex {
                transaction: Arc::new(tx),
                index: 0,
            }],
        };

        let result = execute_batch(batch, &mut deps).await;
        assert!(!result.regular_transactions.is_empty());
        assert!(result.admin_transactions.is_empty());
        assert!(
            result.regular_results.is_some(),
            "regular results should be present"
        );
        assert!(
            result.admin_results.is_none(),
            "no admin results for normal tx"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_execute_batch_multiple_normal_transactions() {
        let (accounts_db, _pg) = start_test_postgres().await;
        let (_tx, rx) = mpsc::unbounded_channel();
        let mut deps = get_execution_deps(accounts_db, rx).await;

        let tx1 = create_test_transaction();
        let tx2 = create_test_transaction();
        let batch = ConflictFreeBatch {
            transactions: vec![
                crate::scheduler::TransactionWithIndex {
                    transaction: Arc::new(tx1),
                    index: 0,
                },
                crate::scheduler::TransactionWithIndex {
                    transaction: Arc::new(tx2),
                    index: 1,
                },
            ],
        };

        let result = execute_batch(batch, &mut deps).await;
        assert_eq!(result.regular_transactions.len(), 2);
        assert!(result.admin_transactions.is_empty());
        let results = result.regular_results.unwrap();
        assert_eq!(results.processing_results.len(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_execution_worker_shutdown_exits_cleanly() {
        let (_accounts_db, _pg) = start_test_postgres().await;
        let url = crate::test_helpers::postgres_container_url(&_pg, "test_db").await;

        let (_batch_tx, batch_rx) = mpsc::unbounded_channel::<ConflictFreeBatch>();
        let (_settled_tx, settled_rx) = mpsc::unbounded_channel();
        let (execution_results_tx, _execution_results_rx) = mpsc::unbounded_channel::<(
            LoadAndExecuteSanitizedTransactionsOutput,
            Vec<SanitizedTransaction>,
        )>();
        let shutdown = CancellationToken::new();

        let handle = start_execution_worker(ExecutionArgs {
            batch_rx,
            settled_accounts_rx: settled_rx,
            execution_results_tx,
            accountsdb_connection_url: url,
            shutdown_token: shutdown.clone(),
            metrics: Arc::new(NoopMetrics),
        })
        .await;

        shutdown.cancel();

        let result = tokio::time::timeout(Duration::from_secs(2), handle.handle).await;
        assert!(result.is_ok(), "worker should exit promptly after shutdown");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_execution_worker_channel_closed_exits() {
        let (_accounts_db, _pg) = start_test_postgres().await;
        let url = crate::test_helpers::postgres_container_url(&_pg, "test_db").await;

        let (batch_tx, batch_rx) = mpsc::unbounded_channel::<ConflictFreeBatch>();
        let (_settled_tx, settled_rx) = mpsc::unbounded_channel();
        let (execution_results_tx, _execution_results_rx) = mpsc::unbounded_channel::<(
            LoadAndExecuteSanitizedTransactionsOutput,
            Vec<SanitizedTransaction>,
        )>();
        let shutdown = CancellationToken::new();

        let handle = start_execution_worker(ExecutionArgs {
            batch_rx,
            settled_accounts_rx: settled_rx,
            execution_results_tx,
            accountsdb_connection_url: url,
            shutdown_token: shutdown.clone(),
            metrics: Arc::new(NoopMetrics),
        })
        .await;

        drop(batch_tx);

        // Worker should exit when input channel closes
        let result = tokio::time::timeout(Duration::from_secs(2), handle.handle).await;
        assert!(
            result.is_ok(),
            "worker should exit when input channel is closed"
        );
    }
}
