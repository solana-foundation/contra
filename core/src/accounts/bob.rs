/// BOB will always store the latest account state in-memory and will call the
/// AccountsDB whenever there is a cache miss.  You can visualize the flow as
/// follows:
///
/// Transaction -> Execution -> BOB
///                    |
///                    v
///                Settlement -> BOB
///                    |
///                    v
///               AccountsDB
///
/// Execution will always read/write from BOB.
/// Settlement still will always write to the AccountsDB.
/// After settlement, we send a message to BOB with the account that we flushed
/// to disk.
///
/// For every account stored in BOB, we also track a field called
/// `synced_since`. This is an `Option<u64>` that tracks in seconds how long the
/// account stored by BOB has been in sync with the AccountsDB>.
///
/// If `synced_since` is `None`, it means BOB has newer state than the
/// AccountsDB. We can NEVER evict accounts with `synced_since` set to `None`.
///
/// If `synced_since` is `Some(x)`, it means BOB has state that is `x` seconds
/// old. We can evict accounts with `synced_since` set to `Some(x)` if they are
/// older than `OLDEST_SYNCED_ACCOUNT_AGE` seconds. Generally, hot accounts will
/// have their `synced_since` updated frequently, so this is a good heuristic to
/// evict less frequently accessed accounts.
use {
    crate::{
        accounts::{bpf_loader_program_account, AccountsDB},
        stages::AccountSettlement,
    },
    solana_sdk::{
        account::{Account, AccountSharedData, ReadableAccount},
        pubkey::Pubkey,
        rent::Rent,
        transaction::SanitizedTransaction,
    },
    solana_svm::{
        transaction_processing_result::ProcessedTransaction,
        transaction_processor::LoadAndExecuteSanitizedTransactionsOutput,
    },
    solana_svm_callback::{InvokeContextCallback, TransactionProcessingCallback},
    solana_svm_transaction::svm_message::SVMMessage,
    std::{
        collections::HashMap,
        time::{SystemTime, UNIX_EPOCH},
    },
    tokio::sync::mpsc,
    tracing::{debug, info, warn},
};

// TODO: Make this a config parameter
const OLDEST_SYNCED_ACCOUNT_AGE: u64 = 60 * 60; // 1 hour
struct AccountWithMeta {
    account: AccountSharedData,
    // TODO: Implement this after we move settlement to a separate stage
    #[allow(dead_code)]
    synced_since: Option<u64>,
    // Whether we deleted this account. We can't remove an account from the
    // HashMap while we keep it in-memory because it will fallback to the
    // AccountsDB.
    deleted: bool,
}

pub struct BOB {
    /// The in-memory account state
    accounts: HashMap<Pubkey, AccountWithMeta>,
    /// Precompiles that are always kept in memory (never garbage collected)
    precompiles: HashMap<Pubkey, AccountSharedData>,
    /// Accounts that are coming from settlement
    settled_accounts_rx: mpsc::UnboundedReceiver<Vec<(Pubkey, AccountSettlement)>>,
    /// AccountsDB account state
    pub accounts_db: AccountsDB,
}

impl BOB {
    pub async fn new(
        accounts_db: AccountsDB,
        settled_accounts_rx: mpsc::UnboundedReceiver<Vec<(Pubkey, AccountSettlement)>>,
    ) -> Self {
        // Initialize precompiles that are always kept in memory
        let mut precompiles = HashMap::new();

        // Use zero rent for gasless operation
        let rent = Rent {
            lamports_per_byte_year: 0,
            exemption_threshold: 0.0,
            burn_percent: 0,
        };

        // Load system program
        let system_account = Account {
            lamports: 0,
            data: b"solana_system_program".to_vec(),
            owner: solana_sdk_ids::native_loader::ID,
            executable: true,
            rent_epoch: u64::MAX,
        };
        precompiles.insert(
            solana_sdk_ids::system_program::ID,
            AccountSharedData::from(system_account),
        );
        info!("Loaded system program");

        // Load SPL Token program
        let spl_token_elf = include_bytes!("../../precompiles/spl_token-8.0.0.so");
        let (spl_token_id, spl_token_account) =
            bpf_loader_program_account(&spl_token::ID, spl_token_elf, &rent);
        precompiles.insert(spl_token_id, AccountSharedData::from(spl_token_account));
        info!("Loaded SPL Token program");

        // Load Associated Token Account program
        let ata_elf = include_bytes!("../../precompiles/spl_associated_token_account-1.1.1.so");
        let (ata_id, ata_account) =
            bpf_loader_program_account(&spl_associated_token_account::ID, ata_elf, &rent);
        precompiles.insert(ata_id, AccountSharedData::from(ata_account));
        info!("Loaded Associated Token Account program");

        // Load rent sysvar
        let rent_account = Account {
            lamports: 0,
            data: bincode::serialize(&rent).unwrap(),
            owner: solana_sdk_ids::sysvar::ID,
            executable: false,
            rent_epoch: u64::MAX,
        };
        precompiles.insert(
            solana_sdk_ids::sysvar::rent::ID,
            AccountSharedData::from(rent_account),
        );
        info!("Loaded rent sysvar");

        // Load Contra Withdraw program
        let withdraw_elf = include_bytes!("../../precompiles/contra_withdraw_program.so");
        // Convert from solana_pubkey::Pubkey to solana_sdk::pubkey::Pubkey
        let (_, withdraw_account) = bpf_loader_program_account(
            &contra_withdraw_program_client::CONTRA_WITHDRAW_PROGRAM_ID,
            withdraw_elf,
            &rent,
        );
        precompiles.insert(
            contra_withdraw_program_client::CONTRA_WITHDRAW_PROGRAM_ID,
            AccountSharedData::from(withdraw_account),
        );
        info!("Loaded Contra Withdraw program");

        Self {
            accounts: HashMap::new(),
            precompiles,
            settled_accounts_rx,
            accounts_db,
        }
    }

    pub fn precompiles(&self) -> &HashMap<Pubkey, AccountSharedData> {
        &self.precompiles
    }

    pub async fn preload_accounts(&mut self, pubkeys: &[Pubkey]) {
        // First, process any settled accounts and perform garbage collection
        self.garbage_collect();

        // Filter out precompiles since they're already in memory
        let accounts_to_load: Vec<Pubkey> = pubkeys
            .iter()
            .filter(|pubkey| !self.precompiles.contains_key(pubkey))
            .copied()
            .collect();

        if accounts_to_load.is_empty() {
            return;
        }

        let accounts = self.accounts_db.get_accounts(&accounts_to_load).await;
        for (index, account_opt) in accounts.iter().enumerate() {
            if let Some(account) = account_opt {
                let pubkey = accounts_to_load[index];
                // Only load in the account if it DNE in-memory
                self.accounts
                    .entry(pubkey)
                    .or_insert_with(|| AccountWithMeta {
                        account: account.clone(),
                        synced_since: None,
                        deleted: false,
                    });
            }
        }
    }

    // TODO: Merge this implementation with the one in the settlement stage
    /// Called to update the accounts in memory
    pub fn update_accounts(
        &mut self,
        svm_output: &LoadAndExecuteSanitizedTransactionsOutput,
        transactions: &[SanitizedTransaction],
    ) {
        for (tx_index, tx) in svm_output.processing_results.iter().enumerate() {
            let sanitized_transaction = &transactions[tx_index];
            let signature = sanitized_transaction.signature();

            match tx {
                Ok(ProcessedTransaction::Executed(executed_transaction)) => {
                    debug!(
                        "Executed transaction: {:?}",
                        sanitized_transaction.signature()
                    );
                    info!("Executed transaction: {:?}", tx);

                    for (index, (pubkey, account_data)) in executed_transaction
                        .loaded_transaction
                        .accounts
                        .iter()
                        .enumerate()
                    {
                        if sanitized_transaction.is_writable(index) {
                            if account_data.lamports() == 0 && account_data.data().is_empty() {
                                self.accounts.insert(
                                    *pubkey,
                                    AccountWithMeta {
                                        account: account_data.clone(),
                                        deleted: true,
                                        synced_since: None,
                                    },
                                );
                            } else {
                                self.accounts.insert(
                                    *pubkey,
                                    AccountWithMeta {
                                        account: account_data.clone(),
                                        deleted: false,
                                        synced_since: None,
                                    },
                                );
                            }
                        }
                    }
                }
                Ok(ProcessedTransaction::FeesOnly(fees_only_transaction)) => {
                    warn!("FeesOnly transaction: {:?}", fees_only_transaction);
                }
                Err(e) => {
                    warn!("Transaction failed: {:?}, error: {:?}", signature, e);
                }
            }
        }
    }

    /// Drain the settled accounts channel and remove accounts that are in sync with the AccountsDB
    fn garbage_collect(&mut self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        while let Ok(account_settlements) = self.settled_accounts_rx.try_recv() {
            for (pubkey, account_settlement) in account_settlements {
                if account_settlement.deleted {
                    // We expect the account to exist in-memory because we only
                    // tombstone deleted accounts
                    if let Some(account) = self.accounts.get(&pubkey) {
                        if account.deleted {
                            self.accounts.remove(&pubkey);
                        }
                    } else {
                        warn!("Account {} was deleted from in-memory, but we expected it to be tombstoned", pubkey);
                    }
                } else if let Some(account) = self.accounts.get_mut(&pubkey) {
                    if account.deleted || account.account != account_settlement.account {
                        // In-memory is ahead of the AccountsDB
                        continue;
                    } else {
                        account.synced_since = Some(now);
                    }
                } else {
                    warn!(
                        "Account {} was deleted from in-memory, but we expected it to be there",
                        pubkey
                    );
                }
            }
        }
        self.accounts.retain(|_pubkey, account| {
            if let Some(synced_since) = account.synced_since {
                synced_since + OLDEST_SYNCED_ACCOUNT_AGE >= now
            } else {
                true // Always keep accounts with synced_since = None
            }
        });
    }
}

impl InvokeContextCallback for BOB {}

impl TransactionProcessingCallback for BOB {
    fn get_account_shared_data(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
        // First check precompiles (always in memory)
        if let Some(precompile) = self.precompiles.get(pubkey) {
            return Some(precompile.clone());
        }

        // Then check in-memory accounts
        if let Some(account) = self.accounts.get(pubkey) {
            if account.deleted {
                return None;
            }
            return Some(account.account.clone());
        }

        None
    }

    fn account_matches_owners(&self, account: &Pubkey, owners: &[Pubkey]) -> Option<usize> {
        self.get_account_shared_data(account)
            .and_then(|account| owners.iter().position(|key| account.owner().eq(key)))
    }
}
