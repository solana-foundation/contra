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
        accounts::{precompiles::PRECOMPILES, AccountsDB},
        stages::AccountSettlement,
    },
    solana_sdk::{
        account::{AccountSharedData, ReadableAccount},
        pubkey::Pubkey,
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
    tracing::{debug, warn},
};

// TODO: Make this a config parameter
const OLDEST_SYNCED_ACCOUNT_AGE: u64 = 60 * 60; // 1 hour

/// Upper bound on resident cache entries. Once exceeded, the oldest clean
/// entries are evicted down to a low watermark. Must stay well above the max
/// distinct account keys a single batch can reference so a batch never evicts
/// its own working set. At ~1 KiB/account this bounds the clean set near ~1 GiB.
const DEFAULT_MAX_CACHE_ENTRIES: usize = 1_000_000;

/// Snapshot of BOB cache size, reported to metrics once per batch.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct BobCacheStats {
    /// Total resident entries (exact, current).
    pub entries: usize,
    /// Resident entries that are ahead of the DB (`synced_since == None`).
    pub dirty_entries: usize,
    /// Resident account-data bytes across all entries.
    pub bytes: usize,
    /// Entries evicted (age + cap) since the previous read; drained on read.
    pub evicted: usize,
}

struct AccountWithMeta {
    account: AccountSharedData,
    synced_since: Option<u64>,
    // Whether we deleted this account. We can't remove an account from the
    // HashMap while we keep it in-memory because it will fallback to the
    // AccountsDB.
    deleted: bool,
}

/// How often (in batches) to run the expensive eviction sweep in garbage_collect.
/// The settled_accounts channel is still drained on every preload to keep
/// dirty/clean tracking current; only the O(N) `retain()` scan is deferred.
const GC_EVICTION_INTERVAL: u64 = 100;

pub struct BOB {
    /// The in-memory account state
    accounts: HashMap<Pubkey, AccountWithMeta>,
    /// Precompiles that are always kept in memory (never garbage collected)
    precompiles: HashMap<Pubkey, AccountSharedData>,
    /// Accounts that are coming from settlement
    settled_accounts_rx: mpsc::UnboundedReceiver<Vec<(Pubkey, AccountSettlement)>>,
    /// AccountsDB account state
    pub accounts_db: AccountsDB,
    /// Counts preload calls since last eviction sweep
    batches_since_eviction: u64,
    /// Hard cap on resident entries; the cap evicts clean entries above it.
    max_cache_entries: usize,
    /// Entries evicted since the last cache_stats read; drained on read.
    evicted_delta: usize,
    /// Running count of dirty (ahead-of-DB) entries. Maintained incrementally so
    /// cache_stats is O(1), and reconciled against truth on the periodic sweep.
    dirty_entries: usize,
    /// Running sum of resident account-data bytes; maintained like dirty_entries.
    cache_bytes: usize,
}

impl BOB {
    pub async fn new(
        accounts_db: AccountsDB,
        settled_accounts_rx: mpsc::UnboundedReceiver<Vec<(Pubkey, AccountSettlement)>>,
    ) -> Self {
        // Precompile ELFs are parsed once process-wide; see
        // `crate::accounts::precompiles`. Cloning the map is cheap, the
        // heavy work of parsing BPF bytes happens at first LazyLock access.
        Self {
            accounts: HashMap::new(),
            precompiles: PRECOMPILES.clone(),
            settled_accounts_rx,
            accounts_db,
            batches_since_eviction: 0,
            max_cache_entries: DEFAULT_MAX_CACHE_ENTRIES,
            evicted_delta: 0,
            dirty_entries: 0,
            cache_bytes: 0,
        }
    }

    pub fn precompiles(&self) -> &HashMap<Pubkey, AccountSharedData> {
        &self.precompiles
    }

    /// Preloads accounts into BOB from the database.
    ///
    /// Returns `(fetched, cached)` where:
    /// - `fetched` = accounts that were missing from BOB and loaded from the DB.
    /// - `cached`  = accounts that were already warm in BOB (no DB round-trip needed).
    ///
    /// Only queries the database for accounts that are actual cache misses
    /// (not in BOB's HashMap and not a precompile). Once the working set is
    /// warm, most batches will skip the DB entirely.
    pub async fn preload_accounts(&mut self, pubkeys: &[Pubkey]) -> (usize, usize) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Split the keys into cache hits and misses, skipping precompiles (always
        // in memory, no DB lookup). Each hit is stamped with the current time so
        // that when eviction runs below, this batch's accounts look freshest and
        // are never the ones evicted. Misses are fetched from the DB further down.
        let mut already_cached = 0usize;
        let mut miss_keys: Vec<Pubkey> = Vec::new();

        for pubkey in pubkeys {
            if self.precompiles.contains_key(pubkey) {
                continue;
            }
            if let Some(entry) = self.accounts.get_mut(pubkey) {
                already_cached += 1;
                // Reading a clean (in-sync) entry does not desync it, so refresh
                // its recency; this keeps hot read-only accounts from aging or
                // capping out.
                if entry.synced_since.is_some() {
                    entry.synced_since = Some(now);
                }
            } else {
                miss_keys.push(*pubkey);
            }
        }

        // Drain settled_accounts to keep dirty/clean tracking current, enforce
        // the hard cap, and run the periodic age sweep. Misses are not resident
        // yet, so they are never eviction candidates here.
        self.garbage_collect();

        // If everything is warm, skip the DB round-trip entirely.
        if miss_keys.is_empty() {
            return (0, already_cached);
        }

        // Only fetch the cache-miss keys from the database.
        let accounts = self.accounts_db.get_accounts(&miss_keys).await;
        let mut fetched = 0usize;
        for (index, account_opt) in accounts.iter().enumerate() {
            if let Some(account) = account_opt {
                // A DB-loaded account is byte-identical to the DB, so it is
                // clean and belongs in the normally-evictable population.
                // Reserve synced_since=None for execution-produced state.
                let meta = AccountWithMeta {
                    account: account.clone(),
                    synced_since: Some(now),
                    deleted: false,
                };
                self.note_added(&meta);
                if let Some(old) = self.accounts.insert(miss_keys[index], meta) {
                    self.note_removed(&old);
                }
                fetched += 1;
            }
        }

        (fetched, already_cached)
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

                    // A failed executed tx (status Err) holds rolled-back intermediate state; only a successful execution commits its writable accounts.
                    if executed_transaction.was_successful() {
                        for (index, (pubkey, account_data)) in executed_transaction
                            .loaded_transaction
                            .accounts
                            .iter()
                            .enumerate()
                        {
                            if sanitized_transaction.is_writable(index) {
                                // A zero-lamport, empty-data writable account is a
                                // tombstone; either way the written state is ahead
                                // of the DB, so it goes in as dirty.
                                let deleted =
                                    account_data.lamports() == 0 && account_data.data().is_empty();
                                let meta = AccountWithMeta {
                                    account: account_data.clone(),
                                    deleted,
                                    synced_since: None,
                                };
                                self.note_added(&meta);
                                if let Some(old) = self.accounts.insert(*pubkey, meta) {
                                    self.note_removed(&old);
                                }
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

    /// Drain the settled accounts channel and periodically evict stale entries.
    ///
    /// Split into two phases:
    /// 1. **Channel drain** (every call): process settled_accounts messages to
    ///    update `synced_since` and remove deleted tombstones. This is lightweight
    ///    — just a `try_recv` loop over whatever messages are pending.
    /// 2. **Eviction sweep** (every `GC_EVICTION_INTERVAL` batches): scan the
    ///    entire HashMap to evict entries that have been synced for longer than
    ///    `OLDEST_SYNCED_ACCOUNT_AGE`. This is O(N) so we avoid it on every batch.
    fn garbage_collect(&mut self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Phase 1: always drain the channel to keep dirty/clean state current.
        while let Ok(account_settlements) = self.settled_accounts_rx.try_recv() {
            for (pubkey, account_settlement) in account_settlements {
                if account_settlement.deleted {
                    // We expect the account to exist in-memory because we only
                    // tombstone deleted accounts
                    match self.accounts.get(&pubkey).map(|account| account.deleted) {
                        Some(true) => {
                            if let Some(old) = self.accounts.remove(&pubkey) {
                                self.note_removed(&old);
                            }
                        }
                        Some(false) => {}
                        None => {
                            warn!("Account {} was deleted from in-memory, but we expected it to be tombstoned", pubkey);
                        }
                    }
                } else if let Some(account) = self.accounts.get_mut(&pubkey) {
                    if account.deleted || account.account != account_settlement.account {
                        // In-memory is ahead of the AccountsDB
                        continue;
                    }
                    // The settled bytes match, so this dirty entry is now clean.
                    let was_dirty = account.synced_since.is_none();
                    account.synced_since = Some(now);
                    if was_dirty {
                        self.dirty_entries = self.dirty_entries.saturating_sub(1);
                    }
                } else {
                    warn!(
                        "Account {} was deleted from in-memory, but we expected it to be there",
                        pubkey
                    );
                }
            }
        }

        // Phase 1b: apply the hard entry cap. The len() check is cheap and skips
        // out when under the cap, so the extra work only happens when the cache
        // has actually grown too large. This runs before the current batch adds
        // its missed accounts, so those accounts are never evicted here.
        self.cap_evict();

        // Phase 2: only run the O(N) eviction sweep periodically. The same scan
        // recomputes the dirty/byte totals from scratch, correcting any drift in
        // the incremental accounting at least once per sweep.
        self.batches_since_eviction += 1;
        if self.batches_since_eviction >= GC_EVICTION_INTERVAL {
            self.batches_since_eviction = 0;
            let before = self.accounts.len();
            let mut dirty = 0usize;
            let mut bytes = 0usize;
            self.accounts.retain(|_pubkey, account| {
                let keep = match account.synced_since {
                    Some(synced_since) => synced_since + OLDEST_SYNCED_ACCOUNT_AGE >= now,
                    None => true, // Always keep accounts with synced_since = None
                };
                if keep {
                    if account.synced_since.is_none() {
                        dirty += 1;
                    }
                    bytes += account.account.data().len();
                }
                keep
            });
            self.evicted_delta += before.saturating_sub(self.accounts.len());
            self.dirty_entries = dirty;
            self.cache_bytes = bytes;
        }
    }

    /// Evict the oldest clean entries when the cache exceeds `max_cache_entries`,
    /// down to a 90% low watermark. Dirty (ahead-of-DB) entries are never
    /// evicted, so the cap can only be enforced against clean state. The low
    /// watermark amortizes the sort so it does not run every batch under
    /// sustained pressure.
    fn cap_evict(&mut self) {
        if self.accounts.len() <= self.max_cache_entries {
            return;
        }

        // Keep at least one entry of headroom even for a tiny cap so we trim
        // toward a 90% watermark instead of collapsing the whole clean set.
        let target = (self.max_cache_entries * 9 / 10).max(1);
        let to_remove = self.accounts.len().saturating_sub(target);
        if to_remove == 0 {
            return;
        }

        // Oldest synced_since first; dirty entries are not candidates.
        let mut clean: Vec<(Pubkey, u64)> = self
            .accounts
            .iter()
            .filter_map(|(pubkey, meta)| meta.synced_since.map(|t| (*pubkey, t)))
            .collect();

        let remove_count = to_remove.min(clean.len());
        if remove_count == 0 {
            return;
        }
        // Partition the oldest `remove_count` entries to the front in O(n)
        // without a full sort. When remove_count == clean.len() every clean entry
        // is being evicted, so we skip the partition and drain the whole vec.
        if remove_count < clean.len() {
            clean.select_nth_unstable_by_key(remove_count, |(_, synced_since)| *synced_since);
        }
        for (pubkey, _) in clean.iter().take(remove_count) {
            if let Some(old) = self.accounts.remove(pubkey) {
                self.note_removed(&old);
            }
        }
        self.evicted_delta += remove_count;
    }

    /// Account the byte and dirty totals for an entry entering the cache.
    fn note_added(&mut self, meta: &AccountWithMeta) {
        self.cache_bytes += meta.account.data().len();
        if meta.synced_since.is_none() {
            self.dirty_entries += 1;
        }
    }

    /// Reverse `note_added` for an entry leaving the cache. Saturating so that a
    /// drift in these metrics-only totals can never underflow and panic.
    fn note_removed(&mut self, meta: &AccountWithMeta) {
        self.cache_bytes = self.cache_bytes.saturating_sub(meta.account.data().len());
        if meta.synced_since.is_none() {
            self.dirty_entries = self.dirty_entries.saturating_sub(1);
        }
    }

    /// Return the current cache-size snapshot and drain the evicted counter.
    /// The dirty and byte totals are maintained incrementally, so this is O(1).
    pub fn cache_stats(&mut self) -> BobCacheStats {
        let stats = BobCacheStats {
            entries: self.accounts.len(),
            dirty_entries: self.dirty_entries,
            bytes: self.cache_bytes,
            evicted: self.evicted_delta,
        };
        self.evicted_delta = 0;
        stats
    }
}

#[cfg(test)]
impl BOB {
    /// Test-only constructor — needs private field access so it lives on the type.
    /// The actual test helper that sets up the dummy DB pool is in test_helpers.rs.
    pub(crate) fn new_test(
        settled_accounts_rx: mpsc::UnboundedReceiver<Vec<(Pubkey, AccountSettlement)>>,
        accounts_db: AccountsDB,
    ) -> Self {
        Self {
            accounts: HashMap::new(),
            precompiles: HashMap::new(),
            settled_accounts_rx,
            accounts_db,
            batches_since_eviction: 0,
            max_cache_entries: DEFAULT_MAX_CACHE_ENTRIES,
            evicted_delta: 0,
            dirty_entries: 0,
            cache_bytes: 0,
        }
    }

    /// Insert an account directly into BOB's cache (test-only).
    pub(crate) fn insert_account_for_test(&mut self, pubkey: Pubkey, account: AccountSharedData) {
        let meta = AccountWithMeta {
            account,
            synced_since: None,
            deleted: false,
        };
        self.note_added(&meta);
        if let Some(old) = self.accounts.insert(pubkey, meta) {
            self.note_removed(&old);
        }
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

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::test_helpers::create_test_sanitized_transaction,
        solana_sdk::account::Account,
        solana_sdk::signature::{Keypair, Signer},
        solana_svm::account_loader::LoadedTransaction,
        solana_svm::transaction_error_metrics::TransactionErrorMetrics,
        solana_svm::transaction_execution_result::{
            ExecutedTransaction, TransactionExecutionDetails,
        },
        solana_svm_callback::TransactionProcessingCallback,
        solana_timings::ExecuteTimings,
    };

    fn create_test_bob() -> (BOB, mpsc::UnboundedSender<Vec<(Pubkey, AccountSettlement)>>) {
        crate::test_helpers::create_test_bob()
    }

    fn make_account(lamports: u64, data: &[u8], owner: &Pubkey) -> AccountSharedData {
        AccountSharedData::from(Account {
            lamports,
            data: data.to_vec(),
            owner: *owner,
            executable: false,
            rent_epoch: 0,
        })
    }

    /// A token-like data account whose balance lives in its data (first 8 bytes), at the 1-lamport floor.
    fn token_like(balance: u64) -> AccountSharedData {
        make_account(1, &balance.to_le_bytes(), &spl_token::id())
    }

    /// Decode the `token_like` balance from an account's data.
    fn token_balance(account: &AccountSharedData) -> u64 {
        u64::from_le_bytes(account.data()[..8].try_into().unwrap())
    }

    /// Build a single-tx Executed processing result with the given status.
    fn executed_with_status(
        status: Result<(), solana_transaction_error::TransactionError>,
        accounts: Vec<(Pubkey, AccountSharedData)>,
    ) -> ProcessedTransaction {
        ProcessedTransaction::Executed(Box::new(ExecutedTransaction {
            loaded_transaction: LoadedTransaction {
                accounts,
                ..Default::default()
            },
            execution_details: TransactionExecutionDetails {
                status,
                log_messages: None,
                inner_instructions: None,
                return_data: None,
                executed_units: 0,
                accounts_data_len_delta: 0,
            },
            programs_modified_by_tx: HashMap::new(),
        }))
    }

    /// Wrap one processing result into a single-tx SVM output.
    fn svm_output(result: ProcessedTransaction) -> LoadAndExecuteSanitizedTransactionsOutput {
        LoadAndExecuteSanitizedTransactionsOutput {
            processing_results: vec![Ok(result)],
            error_metrics: TransactionErrorMetrics::default(),
            execute_timings: ExecuteTimings::default(),
            balance_collector: None,
        }
    }

    /// A failed executed tx must not overwrite a preloaded data account X=10 with its intermediate X=6, nor create dest.
    #[tokio::test]
    async fn failed_executed_does_not_overwrite_existing() {
        let (mut bob, _settled_tx) = create_test_bob();
        let from = Keypair::new();
        let x = from.pubkey();
        let dest = Pubkey::new_unique();
        bob.insert_account_for_test(x, token_like(10));

        // idx 0 (x, fee payer) and idx 1 (dest) are writable in a transfer.
        let tx = create_test_sanitized_transaction(&from, &dest, 0);
        let output = svm_output(executed_with_status(
            Err(
                solana_transaction_error::TransactionError::InstructionError(
                    1,
                    solana_sdk::instruction::InstructionError::Custom(0),
                ),
            ),
            vec![(x, token_like(6)), (dest, token_like(4))],
        ));
        bob.update_accounts(&output, std::slice::from_ref(&tx));

        assert_eq!(
            token_balance(&bob.get_account_shared_data(&x).expect("X must remain")),
            10,
            "failed tx must not overwrite X with its rolled-back intermediate balance"
        );
        assert!(
            bob.get_account_shared_data(&dest).is_none(),
            "dest from a failed tx must not be created"
        );
    }

    /// Success path is unchanged: a successful executed tx still commits X=6 and dest=4 (guards against an inverted guard).
    #[tokio::test]
    async fn successful_executed_still_writes() {
        let (mut bob, _settled_tx) = create_test_bob();
        let from = Keypair::new();
        let x = from.pubkey();
        let dest = Pubkey::new_unique();
        bob.insert_account_for_test(x, token_like(10));

        let tx = create_test_sanitized_transaction(&from, &dest, 0);
        let output = svm_output(executed_with_status(
            Ok(()),
            vec![(x, token_like(6)), (dest, token_like(4))],
        ));
        bob.update_accounts(&output, std::slice::from_ref(&tx));

        assert_eq!(
            token_balance(&bob.get_account_shared_data(&x).expect("X present")),
            6,
            "successful tx commits the new X balance"
        );
        assert_eq!(
            token_balance(&bob.get_account_shared_data(&dest).expect("dest present")),
            4,
            "successful tx commits the new dest balance"
        );
    }

    /// Admin-trap regression: a failed executed result with empty loaded accounts must leave a pre-existing fee payer untouched.
    #[tokio::test]
    async fn failed_executed_does_not_tombstone_fee_payer() {
        let (mut bob, _settled_tx) = create_test_bob();
        let from = Keypair::new();
        let fee_payer = from.pubkey();
        bob.insert_account_for_test(fee_payer, token_like(10));

        let tx = create_test_sanitized_transaction(&from, &Pubkey::new_unique(), 0);
        let output = svm_output(executed_with_status(
            Err(
                solana_transaction_error::TransactionError::InstructionError(
                    0,
                    solana_sdk::instruction::InstructionError::Custom(0),
                ),
            ),
            vec![],
        ));
        bob.update_accounts(&output, std::slice::from_ref(&tx));

        assert_eq!(
            token_balance(
                &bob.get_account_shared_data(&fee_payer)
                    .expect("fee payer present")
            ),
            10,
            "failed executed tx with empty accounts must not tombstone the fee payer"
        );
    }

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    // -----------------------------------------------------------------------
    // Invariant C2: BOB MUST be in sync or ahead of DB on disk
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn gc_marks_matching_account_as_synced() {
        let (mut bob, settled_tx) = create_test_bob();
        let pubkey = Pubkey::new_unique();
        let account = make_account(1000, &[1, 2, 3], &Pubkey::default());

        bob.accounts.insert(
            pubkey,
            AccountWithMeta {
                account: account.clone(),
                synced_since: None,
                deleted: false,
            },
        );

        settled_tx
            .send(vec![(
                pubkey,
                AccountSettlement {
                    account: account.clone(),
                    deleted: false,
                },
            )])
            .unwrap();

        bob.garbage_collect();

        let meta = bob.accounts.get(&pubkey).unwrap();
        assert!(
            meta.synced_since.is_some(),
            "Account matching settler feedback should be marked as synced"
        );
    }

    #[tokio::test]
    async fn gc_preserves_ahead_state_when_data_differs() {
        let (mut bob, settled_tx) = create_test_bob();
        let pubkey = Pubkey::new_unique();
        let newer = make_account(2000, &[9, 9, 9], &Pubkey::default());
        let older = make_account(1000, &[1, 2, 3], &Pubkey::default());

        // Executor wrote newer state to BOB
        bob.accounts.insert(
            pubkey,
            AccountWithMeta {
                account: newer.clone(),
                synced_since: None,
                deleted: false,
            },
        );

        // Settler sends older (now-stale) feedback
        settled_tx
            .send(vec![(
                pubkey,
                AccountSettlement {
                    account: older,
                    deleted: false,
                },
            )])
            .unwrap();

        bob.garbage_collect();

        let meta = bob.accounts.get(&pubkey).unwrap();
        assert_eq!(
            meta.account, newer,
            "In-memory state must not be overwritten by stale settler feedback"
        );
        assert!(
            meta.synced_since.is_none(),
            "Account ahead of DB must keep synced_since=None"
        );
    }

    #[tokio::test]
    async fn gc_preserves_deleted_tombstone_against_non_deleted_settlement() {
        let (mut bob, settled_tx) = create_test_bob();
        let pubkey = Pubkey::new_unique();
        let account = make_account(1000, &[1], &Pubkey::default());

        // BOB has account marked as deleted (tombstone)
        bob.accounts.insert(
            pubkey,
            AccountWithMeta {
                account: account.clone(),
                synced_since: None,
                deleted: true,
            },
        );

        // Settler sends non-deleted settlement (from before the delete)
        settled_tx
            .send(vec![(
                pubkey,
                AccountSettlement {
                    account,
                    deleted: false,
                },
            )])
            .unwrap();

        bob.garbage_collect();

        let meta = bob.accounts.get(&pubkey).unwrap();
        assert!(
            meta.deleted,
            "Deleted tombstone must not be overwritten by stale non-deleted settlement"
        );
        assert!(meta.synced_since.is_none());
    }

    #[tokio::test]
    async fn gc_removes_tombstone_on_deleted_settlement() {
        let (mut bob, settled_tx) = create_test_bob();
        let pubkey = Pubkey::new_unique();
        let account = make_account(0, &[], &Pubkey::default());

        bob.accounts.insert(
            pubkey,
            AccountWithMeta {
                account: account.clone(),
                synced_since: None,
                deleted: true,
            },
        );

        // Settler confirms deletion was persisted to DB
        settled_tx
            .send(vec![(
                pubkey,
                AccountSettlement {
                    account,
                    deleted: true,
                },
            )])
            .unwrap();

        bob.garbage_collect();

        assert!(
            !bob.accounts.contains_key(&pubkey),
            "Tombstoned account must be removed once settler confirms deletion"
        );
    }

    #[tokio::test]
    async fn eviction_never_removes_unsynced_accounts() {
        let (mut bob, _settled_tx) = create_test_bob();
        let pubkey = Pubkey::new_unique();

        bob.accounts.insert(
            pubkey,
            AccountWithMeta {
                account: make_account(1000, &[1], &Pubkey::default()),
                synced_since: None, // Ahead of DB — must never be evicted
                deleted: false,
            },
        );

        bob.garbage_collect();

        assert!(
            bob.accounts.contains_key(&pubkey),
            "Accounts with synced_since=None (ahead of DB) must never be evicted"
        );
    }

    #[tokio::test]
    async fn eviction_removes_old_synced_accounts() {
        let (mut bob, _settled_tx) = create_test_bob();
        let pubkey = Pubkey::new_unique();

        bob.accounts.insert(
            pubkey,
            AccountWithMeta {
                account: make_account(1000, &[1], &Pubkey::default()),
                synced_since: Some(now_secs() - OLDEST_SYNCED_ACCOUNT_AGE - 1),
                deleted: false,
            },
        );

        // Force the eviction sweep to run on the next garbage_collect call
        bob.batches_since_eviction = GC_EVICTION_INTERVAL - 1;
        bob.garbage_collect();

        assert!(
            !bob.accounts.contains_key(&pubkey),
            "Synced accounts older than OLDEST_SYNCED_ACCOUNT_AGE should be evicted"
        );
    }

    #[tokio::test]
    async fn eviction_keeps_recently_synced_accounts() {
        let (mut bob, _settled_tx) = create_test_bob();
        let pubkey = Pubkey::new_unique();

        bob.accounts.insert(
            pubkey,
            AccountWithMeta {
                account: make_account(1000, &[1], &Pubkey::default()),
                synced_since: Some(now_secs()),
                deleted: false,
            },
        );

        bob.garbage_collect();

        assert!(
            bob.accounts.contains_key(&pubkey),
            "Recently synced accounts must not be evicted"
        );
    }

    #[test]
    fn preload_or_insert_preserves_inflight_state() {
        // Directly tests the HashMap pattern used by preload_accounts.
        // Verifies that entry().or_insert_with() does not overwrite
        // an existing in-flight account with stale DB data.
        let mut accounts: HashMap<Pubkey, AccountWithMeta> = HashMap::new();
        let pubkey = Pubkey::new_unique();

        let newer = make_account(2000, &[9, 9], &Pubkey::default());
        let older_from_db = make_account(1000, &[1, 2], &Pubkey::default());

        // Executor already wrote newer state
        accounts.insert(
            pubkey,
            AccountWithMeta {
                account: newer.clone(),
                synced_since: None,
                deleted: false,
            },
        );

        // Simulate what preload_accounts does with DB results
        accounts.entry(pubkey).or_insert_with(|| AccountWithMeta {
            account: older_from_db,
            synced_since: None,
            deleted: false,
        });

        assert_eq!(
            accounts[&pubkey].account, newer,
            "or_insert_with must not overwrite existing in-flight state"
        );
    }

    #[tokio::test]
    async fn concurrent_preload_and_settle_preserves_newer_state() {
        // Simulates the race condition: executor writes v2 after settler
        // sends v1 feedback but before GC runs.
        let (mut bob, settled_tx) = create_test_bob();
        let pubkey = Pubkey::new_unique();

        let v1 = make_account(1000, &[1], &Pubkey::default());
        let v2 = make_account(2000, &[2], &Pubkey::default());

        // Step 1: Executor writes v1 to BOB
        bob.accounts.insert(
            pubkey,
            AccountWithMeta {
                account: v1.clone(),
                synced_since: None,
                deleted: false,
            },
        );

        // Step 2: Settler settles v1 and sends feedback
        settled_tx
            .send(vec![(
                pubkey,
                AccountSettlement {
                    account: v1,
                    deleted: false,
                },
            )])
            .unwrap();

        // Step 3: Before GC runs, executor updates BOB to v2
        bob.accounts.insert(
            pubkey,
            AccountWithMeta {
                account: v2.clone(),
                synced_since: None,
                deleted: false,
            },
        );

        // Step 4: GC runs (triggered by next preload_accounts)
        bob.garbage_collect();

        let meta = bob.accounts.get(&pubkey).unwrap();
        assert_eq!(
            meta.account, v2,
            "GC must not regress to v1 when BOB already has v2"
        );
        assert!(
            meta.synced_since.is_none(),
            "Account ahead of DB must keep synced_since=None"
        );
    }

    #[tokio::test]
    async fn gc_multi_batch_settlement_applies_all_in_order() {
        let (mut bob, settled_tx) = create_test_bob();
        let pubkey = Pubkey::new_unique();

        let v1 = make_account(1000, &[1], &Pubkey::default());
        let v2 = make_account(2000, &[2], &Pubkey::default());
        let v3 = make_account(3000, &[3], &Pubkey::default());

        // BOB has v3 (latest from executor)
        bob.accounts.insert(
            pubkey,
            AccountWithMeta {
                account: v3.clone(),
                synced_since: None,
                deleted: false,
            },
        );

        // Two settlement batches queue up before GC runs
        settled_tx
            .send(vec![(
                pubkey,
                AccountSettlement {
                    account: v1,
                    deleted: false,
                },
            )])
            .unwrap();
        settled_tx
            .send(vec![(
                pubkey,
                AccountSettlement {
                    account: v2,
                    deleted: false,
                },
            )])
            .unwrap();

        bob.garbage_collect();

        let meta = bob.accounts.get(&pubkey).unwrap();
        assert_eq!(
            meta.account, v3,
            "BOB must preserve v3 when neither settlement batch matches"
        );
        assert!(
            meta.synced_since.is_none(),
            "Account ahead of all settled versions must keep synced_since=None"
        );
    }

    #[tokio::test]
    async fn gc_settlement_for_missing_account_does_not_panic() {
        let (mut bob, settled_tx) = create_test_bob();
        let missing_pubkey = Pubkey::new_unique();

        // Settler sends feedback for an account that was never in BOB
        settled_tx
            .send(vec![(
                missing_pubkey,
                AccountSettlement {
                    account: make_account(1000, &[1], &Pubkey::default()),
                    deleted: false,
                },
            )])
            .unwrap();

        // Should not panic; just logs a warning
        bob.garbage_collect();

        assert!(
            !bob.accounts.contains_key(&missing_pubkey),
            "Settlement for missing account must not create a new entry"
        );
    }

    #[tokio::test]
    async fn eviction_mixed_population() {
        let (mut bob, _settled_tx) = create_test_bob();
        let now = now_secs();

        let old_synced = Pubkey::new_unique();
        let recent_synced = Pubkey::new_unique();
        let unsynced = Pubkey::new_unique();

        bob.accounts.insert(
            old_synced,
            AccountWithMeta {
                account: make_account(100, &[1], &Pubkey::default()),
                synced_since: Some(now - OLDEST_SYNCED_ACCOUNT_AGE - 1),
                deleted: false,
            },
        );
        bob.accounts.insert(
            recent_synced,
            AccountWithMeta {
                account: make_account(200, &[2], &Pubkey::default()),
                synced_since: Some(now),
                deleted: false,
            },
        );
        bob.accounts.insert(
            unsynced,
            AccountWithMeta {
                account: make_account(300, &[3], &Pubkey::default()),
                synced_since: None,
                deleted: false,
            },
        );

        // Force the eviction sweep to run on the next garbage_collect call
        bob.batches_since_eviction = GC_EVICTION_INTERVAL - 1;
        bob.garbage_collect();

        assert!(
            !bob.accounts.contains_key(&old_synced),
            "Old synced account must be evicted"
        );
        assert!(
            bob.accounts.contains_key(&recent_synced),
            "Recently synced account must survive eviction"
        );
        assert!(
            bob.accounts.contains_key(&unsynced),
            "Unsynced (ahead of DB) account must survive eviction"
        );
    }

    #[tokio::test]
    async fn eviction_boundary_exact_age_is_kept() {
        let (mut bob, _settled_tx) = create_test_bob();
        let pubkey = Pubkey::new_unique();

        // synced_since + OLDEST_SYNCED_ACCOUNT_AGE == now → should be kept (>= check)
        bob.accounts.insert(
            pubkey,
            AccountWithMeta {
                account: make_account(1000, &[1], &Pubkey::default()),
                synced_since: Some(now_secs() - OLDEST_SYNCED_ACCOUNT_AGE),
                deleted: false,
            },
        );

        bob.garbage_collect();

        assert!(
            bob.accounts.contains_key(&pubkey),
            "Account synced exactly OLDEST_SYNCED_ACCOUNT_AGE ago must be kept (>= boundary)"
        );
    }

    #[tokio::test]
    async fn get_account_returns_none_for_deleted() {
        let (mut bob, _tx) = create_test_bob();
        let pubkey = Pubkey::new_unique();
        bob.accounts.insert(
            pubkey,
            AccountWithMeta {
                account: make_account(1000, &[1], &Pubkey::default()),
                synced_since: None,
                deleted: true,
            },
        );

        assert!(
            TransactionProcessingCallback::get_account_shared_data(&bob, &pubkey).is_none(),
            "Deleted (tombstoned) accounts must return None to SVM"
        );
    }

    #[tokio::test]
    async fn get_account_returns_live_account() {
        let (mut bob, _tx) = create_test_bob();
        let pubkey = Pubkey::new_unique();
        let account = make_account(1000, &[1, 2, 3], &Pubkey::default());

        bob.accounts.insert(
            pubkey,
            AccountWithMeta {
                account: account.clone(),
                synced_since: None,
                deleted: false,
            },
        );

        let result = TransactionProcessingCallback::get_account_shared_data(&bob, &pubkey);
        assert_eq!(
            result.unwrap(),
            account,
            "Live account must be returned to SVM"
        );
    }

    // -----------------------------------------------------------------------
    // Preload recency touch, hard cap, and cache-size stats
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn preload_hit_refreshes_clean_recency() {
        let (mut bob, _settled_tx) = create_test_bob();
        let pubkey = Pubkey::new_unique();
        let now = now_secs();

        bob.accounts.insert(
            pubkey,
            AccountWithMeta {
                account: make_account(1000, &[1], &Pubkey::default()),
                synced_since: Some(now - 1800),
                deleted: false,
            },
        );

        // Everything is cached, so no DB round-trip happens.
        bob.preload_accounts(&[pubkey]).await;

        let meta = bob.accounts.get(&pubkey).unwrap();
        assert!(
            meta.synced_since.unwrap() >= now - 1,
            "a clean cache hit must be restamped to the current time"
        );
    }

    #[tokio::test]
    async fn preload_hit_does_not_sync_dirty() {
        let (mut bob, _settled_tx) = create_test_bob();
        let pubkey = Pubkey::new_unique();

        bob.accounts.insert(
            pubkey,
            AccountWithMeta {
                account: make_account(1000, &[1], &Pubkey::default()),
                synced_since: None,
                deleted: false,
            },
        );

        bob.preload_accounts(&[pubkey]).await;

        let meta = bob.accounts.get(&pubkey).unwrap();
        assert!(
            meta.synced_since.is_none(),
            "reading a dirty (ahead-of-DB) entry must not mark it clean"
        );
    }

    #[tokio::test]
    async fn cap_evicts_oldest_clean_over_limit() {
        let (mut bob, _settled_tx) = create_test_bob();
        bob.max_cache_entries = 10;
        let now = now_secs();

        let mut keys = Vec::new();
        for i in 0..12u64 {
            let pk = Pubkey::new_unique();
            keys.push(pk);
            bob.accounts.insert(
                pk,
                AccountWithMeta {
                    account: make_account(1, &[1], &Pubkey::default()),
                    synced_since: Some(now - 100 + i), // strictly increasing recency
                    deleted: false,
                },
            );
        }

        bob.garbage_collect();

        assert_eq!(
            bob.accounts.len(),
            9,
            "cap evicts down to the 90% low watermark"
        );
        for pk in &keys[..3] {
            assert!(
                !bob.accounts.contains_key(pk),
                "the three oldest clean entries are evicted first"
            );
        }
        for pk in &keys[3..] {
            assert!(
                bob.accounts.contains_key(pk),
                "the newest clean entries survive the cap"
            );
        }
    }

    #[tokio::test]
    async fn cap_never_evicts_dirty() {
        let (mut bob, _settled_tx) = create_test_bob();
        bob.max_cache_entries = 2;

        for _ in 0..3 {
            bob.accounts.insert(
                Pubkey::new_unique(),
                AccountWithMeta {
                    account: make_account(1, &[1], &Pubkey::default()),
                    synced_since: None,
                    deleted: false,
                },
            );
        }

        bob.garbage_collect();

        assert_eq!(
            bob.accounts.len(),
            3,
            "dirty (ahead-of-DB) entries are never evicted, even over the cap"
        );
    }

    /// A clean account referenced by the current batch must survive the cap even
    /// if it was cold, because preload restamps hits before eviction runs.
    #[tokio::test]
    async fn cap_keeps_account_referenced_this_batch() {
        let (mut bob, _settled_tx) = create_test_bob();
        bob.max_cache_entries = 2;
        let stale = now_secs() - 10_000;

        // Two cold clean entries not referenced this batch (evictable).
        for _ in 0..2 {
            bob.accounts.insert(
                Pubkey::new_unique(),
                AccountWithMeta {
                    account: make_account(1, &[1], &Pubkey::default()),
                    synced_since: Some(stale),
                    deleted: false,
                },
            );
        }
        // One equally-cold clean entry that this batch references.
        let referenced = Pubkey::new_unique();
        bob.accounts.insert(
            referenced,
            AccountWithMeta {
                account: make_account(1, &[1], &Pubkey::default()),
                synced_since: Some(stale),
                deleted: false,
            },
        );

        // All keys are cached, so this stays in-memory (no DB round-trip).
        bob.preload_accounts(&[referenced]).await;

        assert!(
            bob.accounts.contains_key(&referenced),
            "an account referenced this batch must not be cap-evicted"
        );
    }

    #[tokio::test]
    async fn cache_stats_reports_and_drains() {
        let (mut bob, _settled_tx) = create_test_bob();
        let now = now_secs();

        // One old clean entry that the sweep will evict.
        bob.accounts.insert(
            Pubkey::new_unique(),
            AccountWithMeta {
                account: make_account(1, &[0u8; 5], &Pubkey::default()),
                synced_since: Some(now - OLDEST_SYNCED_ACCOUNT_AGE - 1),
                deleted: false,
            },
        );
        // Two recent clean entries (kept), data lengths 3 and 7.
        bob.accounts.insert(
            Pubkey::new_unique(),
            AccountWithMeta {
                account: make_account(1, &[1u8; 3], &Pubkey::default()),
                synced_since: Some(now),
                deleted: false,
            },
        );
        bob.accounts.insert(
            Pubkey::new_unique(),
            AccountWithMeta {
                account: make_account(1, &[2u8; 7], &Pubkey::default()),
                synced_since: Some(now),
                deleted: false,
            },
        );
        // One dirty entry (kept), data length 4.
        bob.accounts.insert(
            Pubkey::new_unique(),
            AccountWithMeta {
                account: make_account(1, &[3u8; 4], &Pubkey::default()),
                synced_since: None,
                deleted: false,
            },
        );

        // Force the eviction sweep to run on this call.
        bob.batches_since_eviction = GC_EVICTION_INTERVAL - 1;
        bob.garbage_collect();

        let stats = bob.cache_stats();
        assert_eq!(stats.entries, 3, "one old clean entry was evicted");
        assert_eq!(stats.dirty_entries, 1, "only the None entry is dirty");
        assert_eq!(
            stats.bytes,
            3 + 7 + 4,
            "bytes sums the data length of kept entries"
        );
        assert_eq!(stats.evicted, 1, "one entry was evicted this sweep");

        let drained = bob.cache_stats();
        assert_eq!(drained.evicted, 0, "the evicted delta drains after a read");
    }

    /// dirty_entries and bytes are maintained incrementally, so they are exact
    /// between eviction sweeps, not only right after one.
    #[tokio::test]
    async fn cache_stats_maintained_without_sweep() {
        let (mut bob, settled_tx) = create_test_bob();
        let a = Pubkey::new_unique();
        let account_a = make_account(1, &[0u8; 5], &Pubkey::default());
        bob.insert_account_for_test(a, account_a.clone());
        bob.insert_account_for_test(
            Pubkey::new_unique(),
            make_account(1, &[0u8; 3], &Pubkey::default()),
        );

        // No sweep is forced, so these totals come purely from incremental accounting.
        let stats = bob.cache_stats();
        assert_eq!(stats.entries, 2);
        assert_eq!(stats.dirty_entries, 2, "both inserts are dirty (None)");
        assert_eq!(stats.bytes, 5 + 3);

        // Settling `a` matches its in-memory bytes, flipping it clean with no sweep.
        settled_tx
            .send(vec![(
                a,
                AccountSettlement {
                    account: account_a,
                    deleted: false,
                },
            )])
            .unwrap();
        bob.garbage_collect();

        let stats = bob.cache_stats();
        assert_eq!(stats.dirty_entries, 1, "settling a marks it clean");
        assert_eq!(stats.bytes, 5 + 3, "a clean flip does not change bytes");
        assert_eq!(stats.entries, 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn preload_marks_db_loaded_account_clean() {
        let (mut redis_raw, _redis) = crate::test_helpers::start_test_redis().await;
        let pubkey = Pubkey::new_unique();
        let account = make_account(5000, &[7, 7, 7], &Pubkey::default());
        // Seed Redis using the exact key/serialization get_accounts_redis expects.
        redis_raw.set_account(pubkey, account.clone()).await;

        let db = crate::accounts::AccountsDB::Redis(redis_raw);
        let (_settled_tx, rx) = mpsc::unbounded_channel();
        let mut bob = BOB::new_test(rx, db);

        let now = now_secs();
        let (fetched, cached) = bob.preload_accounts(&[pubkey]).await;
        assert_eq!(
            (fetched, cached),
            (1, 0),
            "account is loaded from the DB as a cache miss"
        );

        let synced = bob
            .accounts
            .get(&pubkey)
            .unwrap()
            .synced_since
            .expect("a DB-loaded account must be marked clean, not pinned");
        assert!(synced >= now - 1, "the clean stamp reflects load time");

        // Backdate past the age cap and force a sweep: a pinned (None) account
        // would survive, so eviction here proves the entry is unpinned.
        bob.accounts.get_mut(&pubkey).unwrap().synced_since =
            Some(now - OLDEST_SYNCED_ACCOUNT_AGE - 1);
        bob.batches_since_eviction = GC_EVICTION_INTERVAL - 1;
        bob.garbage_collect();

        assert!(
            !bob.accounts.contains_key(&pubkey),
            "a clean DB-loaded account is age-evictable, no longer pinned in memory"
        );
    }
}
