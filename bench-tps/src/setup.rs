//! Phase 1 — Setup
//!
//! Prepares all on-chain state the load phase needs:
//!   1. Loads the admin keypair from disk.
//!   2. Generates N fresh account keypairs in parallel (rayon).
//!   3. Creates an SPL mint owned by the admin keypair.
//!   4. Creates an Associated Token Account (ATA) for every keypair.
//!   5. Waits for all ATAs to be confirmed on-chain.
//!   6. Mints an initial token balance to every ATA.
//!   7. Waits for all mint-to transactions to be confirmed.
//!   8. Fetches the current blockhash and seeds `BenchState`.
//!
//! The setup phase must complete successfully before the load phase starts.
//! All transactions use `send_transaction` + `poll_confirmations` rather than
//! `send_and_confirm_transaction` because the contra node confirms
//! asynchronously through its pipeline, which outlasts the client-side
//! blockhash-expiry timeout.

use {
    crate::{
        rpc::{poll_confirmations, send_parallel},
        types::{BenchState, MINT_DECIMALS},
    },
    anyhow::{Context, Result},
    contra_core::client::{
        create_admin_initialize_mint, create_admin_mint_to, create_ata_transaction,
    },
    rayon::prelude::*,
    solana_client::nonblocking::rpc_client::RpcClient,
    solana_sdk::{signature::Keypair, signer::Signer},
    spl_associated_token_account::get_associated_token_address,
    std::{sync::Arc, time::Instant},
    tokio::{sync::RwLock, time::Duration},
    tracing::{info, warn},
};

/// Everything produced by the setup phase that the load phase needs.
pub struct SetupResult {
    /// The SPL mint pubkey — used to derive ATAs and sign transfers.
    pub mint: solana_sdk::pubkey::Pubkey,
    /// Funded keypairs, one per `--accounts`.  Owned by `Arc` so they can be
    /// shared cheaply across the generator and multiple sender threads.
    pub keypairs: Vec<Arc<Keypair>>,
    /// Shared mutable state seeded with the current blockhash.  Handed off to
    /// the blockhash poller, which keeps it fresh throughout the load phase.
    pub state: Arc<BenchState>,
}

/// Run all setup tasks in order and return the results needed by the load phase.
///
/// `rpc_url`        — write-node / gateway endpoint
/// `admin_path`     — path to the admin keypair JSON file
/// `num_accounts`   — how many funded accounts to create
/// `initial_balance`— raw token units minted to each ATA
pub async fn run_setup_phase(
    rpc_url: &str,
    admin_path: &std::path::Path,
    num_accounts: usize,
    initial_balance: u64,
) -> Result<SetupResult> {
    // ------------------------------------------------------------------
    // Task 1: Load admin keypair
    //
    // The admin keypair authorises the mint initialisation, ATA creation,
    // and mint-to transactions.  It must already be funded with enough SOL
    // to pay transaction fees for all setup operations.
    // ------------------------------------------------------------------
    let admin_keypair = Arc::new(
        contra_core::client::load_keypair(admin_path)
            .map_err(|e| anyhow::anyhow!("failed to load admin keypair: {e}"))?,
    );
    info!(pubkey = %admin_keypair.pubkey(), path = %admin_path.display(), "Loaded admin keypair");

    // ------------------------------------------------------------------
    // Task 2: Generate N account keypairs in parallel (rayon)
    //
    // All N keypairs are generated concurrently using the rayon thread pool.
    // Each keypair is wrapped in `Arc` so it can be cheaply cloned into the
    // generator task and sender threads during the load phase.
    // ------------------------------------------------------------------
    let t2 = Instant::now();
    let keypairs: Vec<Arc<Keypair>> = (0..num_accounts)
        .into_par_iter()
        .map(|_| Arc::new(Keypair::new()))
        .collect();
    info!(
        count = keypairs.len(),
        elapsed_ms = t2.elapsed().as_millis(),
        "Generated account keypairs",
    );

    let rpc = RpcClient::new(rpc_url.to_owned());

    // ------------------------------------------------------------------
    // Task 3: Initialise SPL mint
    //
    // A fresh mint keypair is generated each run so there are no conflicts
    // with previous runs.  The mint must be confirmed before ATAs can be
    // created against it.
    //
    // Retry with exponential backoff in case the write-node is still
    // warming up when the bench first runs (common on fresh Docker starts).
    // ------------------------------------------------------------------
    let t3 = Instant::now();
    let mint_keypair = Keypair::new();
    let mint = mint_keypair.pubkey();
    let send_retry_delays: &[u64] = &[1, 2, 4, 8, 16, 30];
    let mint_sig = 'send: {
        let mut last_err = String::new();
        for (attempt, &delay_secs) in send_retry_delays.iter().enumerate() {
            match rpc.get_latest_blockhash().await {
                Err(e) => {
                    warn!(attempt, err = %e, "get_latest_blockhash failed, retrying in {delay_secs}s");
                    last_err = e.to_string();
                }
                Ok(blockhash) => {
                    let init_tx = create_admin_initialize_mint(
                        &admin_keypair,
                        &mint,
                        MINT_DECIMALS,
                        blockhash,
                    );
                    match rpc.send_transaction(&init_tx).await {
                        Ok(sig) => break 'send sig,
                        Err(e) => {
                            warn!(attempt, err = %e, "initialize_mint send failed, retrying in {delay_secs}s");
                            last_err = e.to_string();
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(delay_secs)).await;
        }
        return Err(anyhow::anyhow!(
            "initialize_mint: all retries exhausted: {last_err}"
        ));
    };
    poll_confirmations(&rpc, &[mint_sig], "initialize_mint").await?;
    info!(mint = %mint, elapsed_ms = t3.elapsed().as_millis(), "Mint initialized");

    // ------------------------------------------------------------------
    // Task 4: Create ATAs for all keypairs in parallel
    //
    // All `create_ata_transaction` calls share the same blockhash.  Sending
    // them in chunks of MAX_CONCURRENT_SENDS bounds peak connection count.
    // ------------------------------------------------------------------
    let t4 = Instant::now();
    let blockhash = rpc
        .get_latest_blockhash()
        .await
        .context("get_latest_blockhash")?;
    let ata_sigs = send_parallel(rpc_url, &keypairs, |kp, url| {
        let admin = Arc::clone(&admin_keypair);
        let owner = kp.pubkey();
        async move {
            let tx = create_ata_transaction(&admin, &owner, &mint, blockhash);
            RpcClient::new(url).send_transaction(&tx).await
        }
    })
    .await;
    info!(
        sent = ata_sigs.len(),
        total = keypairs.len(),
        elapsed_ms = t4.elapsed().as_millis(),
        "ATA transactions sent",
    );

    // ------------------------------------------------------------------
    // Task 5: Wait for all ATAs to be confirmed
    //
    // ATAs must exist on-chain before mint-to transactions can reference them.
    // ------------------------------------------------------------------
    let t5 = Instant::now();
    poll_confirmations(&rpc, &ata_sigs, "ATA").await?;
    info!(
        confirmed = ata_sigs.len(),
        elapsed_ms = t5.elapsed().as_millis(),
        "ATAs confirmed",
    );

    // ------------------------------------------------------------------
    // Task 6: Mint initial token balances to all ATAs in parallel
    //
    // Each account receives `initial_balance` raw token units.  With
    // TRANSFER_AMOUNT = 1 per transfer this is also the maximum number of
    // transfers the account can make before its balance hits zero.
    // ------------------------------------------------------------------
    let t6 = Instant::now();
    let blockhash = rpc
        .get_latest_blockhash()
        .await
        .context("get_latest_blockhash")?;
    let mint_sigs = send_parallel(rpc_url, &keypairs, |kp, url| {
        let admin = Arc::clone(&admin_keypair);
        let ata = get_associated_token_address(&kp.pubkey(), &mint);
        async move {
            let tx = create_admin_mint_to(&admin, &mint, &ata, initial_balance, blockhash);
            RpcClient::new(url).send_transaction(&tx).await
        }
    })
    .await;
    info!(
        sent = mint_sigs.len(),
        total = keypairs.len(),
        elapsed_ms = t6.elapsed().as_millis(),
        "Mint-to transactions sent",
    );

    // ------------------------------------------------------------------
    // Task 7: Wait for all mint-to transactions to be confirmed
    // ------------------------------------------------------------------
    let t7 = Instant::now();
    poll_confirmations(&rpc, &mint_sigs, "mint-to").await?;
    info!(
        confirmed = mint_sigs.len(),
        elapsed_ms = t7.elapsed().as_millis(),
        "Mint-to confirmed",
    );

    // ------------------------------------------------------------------
    // Task 8: Seed BenchState with the current blockhash
    //
    // The blockhash poller will keep this value fresh during the load phase.
    // Using the latest hash at the end of setup avoids an instant stale-hash
    // rejection on the very first batch of transfers.
    // ------------------------------------------------------------------
    let t8 = Instant::now();
    let initial_blockhash = rpc
        .get_latest_blockhash()
        .await
        .context("get_latest_blockhash")?;
    let state = Arc::new(BenchState {
        current_blockhash: RwLock::new(initial_blockhash),
    });
    info!(
        blockhash = %initial_blockhash,
        elapsed_ms = t8.elapsed().as_millis(),
        "Initial blockhash seeded — setup complete",
    );

    Ok(SetupResult {
        mint,
        keypairs,
        state,
    })
}
