//! Withdraw setup phase — L1 escrow bootstrap + L2 mint preparation.
//!
//! Creates all on-chain state needed for the full e2e withdraw load test
//! **without** running operator-solana or waiting for deposits to land.
//!
//! L1 (Solana validator / escrow program):
//!   1. Load admin keypair and instance-seed keypair.
//!   2. CreateInstance — initialises the escrow instance PDA.
//!   3. AddOperator   — registers admin as the ReleaseFunds operator.
//!   4. Create L1 SPL mint (admin = mint authority, same keypair reused on L2).
//!   5. AllowMint     — registers the mint; creates allowed_mint PDA and
//!      instance ATA on L1.
//!   6. Seed the instance ATA with `num_accounts × initial_balance` tokens so
//!      ReleaseFunds has enough tokens to release for every withdrawal.
//!
//! L2 (contra write-node):
//!   7. Initialize the same mint on L2 (same pubkey, implicit account creation).
//!   8. Create L2 ATAs for each withdrawer account.
//!   9. Mint `initial_balance` tokens to each L2 ATA.
//!
//! This bypasses operator-solana entirely — setup completes in seconds rather
//! than waiting for the full deposit → mint pipeline.

use {
    crate::{
        rpc::{poll_confirmations, send_parallel},
        setup_deposit::find_instance_pda,
        types::{BenchState, WithdrawConfig, MINT_DECIMALS, SETUP_BATCH_SIZE},
    },
    anyhow::{Context, Result},
    contra_core::client::{
        create_admin_initialize_mint, create_admin_mint_to, create_ata_transaction,
    },
    contra_escrow_program_client::{
        instructions::{
            AddOperator, AddOperatorInstructionArgs, AllowMint, AllowMintInstructionArgs,
            CreateInstance, CreateInstanceInstructionArgs,
        },
        CONTRA_ESCROW_PROGRAM_ID,
    },
    rayon::prelude::*,
    solana_client::{nonblocking::rpc_client::RpcClient, rpc_config::RpcSendTransactionConfig},
    solana_sdk::{
        commitment_config::CommitmentConfig, pubkey::Pubkey, signature::Keypair, signer::Signer,
        transaction::Transaction,
    },
    solana_system_interface::{instruction as system_instruction, program},
    spl_associated_token_account::get_associated_token_address,
    spl_token::{solana_program::program_pack::Pack, state::Mint as SplMint},
    std::{path::Path, sync::Arc, time::Instant},
    tokio::{sync::RwLock, time::Duration},
    tracing::{info, warn},
};

const ALLOWED_MINT_SEED_PREFIX: &[u8] = b"allowed_mint";
const EVENT_AUTHORITY_SEED: &[u8] = b"event_authority";
const OPERATOR_SEED: &[u8] = b"operator";

/// 10 SOL minimum on L1 (covers CreateInstance + AddOperator + AllowMint fees
/// plus seed ATA mint-to fees).
const MIN_ADMIN_LAMPORTS: u64 = 10_000_000_000;
/// 100 SOL top-up
const AIRDROP_LAMPORTS: u64 = 100_000_000_000;

fn find_allowed_mint_pda(instance_pda: &Pubkey, mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[
            ALLOWED_MINT_SEED_PREFIX,
            instance_pda.as_ref(),
            mint.as_ref(),
        ],
        &CONTRA_ESCROW_PROGRAM_ID,
    )
}

fn find_event_authority() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[EVENT_AUTHORITY_SEED], &CONTRA_ESCROW_PROGRAM_ID)
}

fn find_operator_pda(instance_pda: &Pubkey, operator: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[OPERATOR_SEED, instance_pda.as_ref(), operator.as_ref()],
        &CONTRA_ESCROW_PROGRAM_ID,
    )
}

/// Run the full withdraw setup phase and return the `WithdrawConfig` needed by
/// the withdraw load phase.
///
/// `l1_rpc_url`         — L1 Solana validator RPC endpoint
/// `l2_rpc_url`         — L2 contra write-node / gateway RPC endpoint
/// `admin_path`         — path to the admin keypair JSON file
/// `instance_seed_path` — optional path to save/load the instance-seed keypair;
///                        reuse the same file as the deposit bench so that
///                        operator-contra (pre-configured with the matching PDA)
///                        can observe the resulting ReleaseFunds calls.
/// `num_accounts`       — number of L2 withdrawer accounts to create
/// `initial_balance`    — raw token units minted to each L2 withdrawer ATA;
///                        also determines the total seed amount for the L1
///                        instance ATA (`num_accounts × initial_balance`).
pub async fn run_setup_withdraw_phase(
    l1_rpc_url: &str,
    l2_rpc_url: &str,
    admin_path: &Path,
    instance_seed_path: Option<&Path>,
    num_accounts: usize,
    initial_balance: u64,
) -> Result<WithdrawConfig> {
    // ------------------------------------------------------------------
    // Task 1: Load admin keypair
    // ------------------------------------------------------------------
    let admin_keypair = Arc::new(
        contra_core::client::load_keypair(admin_path)
            .map_err(|e| anyhow::anyhow!("failed to load admin keypair: {e}"))?,
    );
    info!(pubkey = %admin_keypair.pubkey(), "Loaded admin keypair (withdraw setup)");

    // ------------------------------------------------------------------
    // Task 2: Load or generate the instance-seed keypair and derive PDAs
    //
    // Reuse the same keypair file as the deposit bench so that
    // COMMON_ESCROW_INSTANCE_ID in docker-compose matches what we create here.
    // ------------------------------------------------------------------
    let instance_seed_keypair: Keypair = match instance_seed_path {
        Some(path) if path.exists() => contra_core::client::load_keypair(path)
            .map_err(|e| anyhow::anyhow!("failed to load instance-seed keypair: {e}"))?,
        Some(path) => {
            let kp = Keypair::new();
            let bytes = kp.to_bytes();
            let json = serde_json::to_string(&bytes.to_vec())
                .context("serialize instance-seed keypair")?;
            std::fs::write(path, json).context("write instance-seed keypair")?;
            info!(path = %path.display(), "Generated and saved new instance-seed keypair");
            kp
        }
        None => Keypair::new(),
    };
    let instance_seed_pubkey = instance_seed_keypair.pubkey();
    let (instance_pda, instance_bump) = find_instance_pda(&instance_seed_pubkey);
    let (event_authority, _) = find_event_authority();
    let (operator_pda, operator_bump) = find_operator_pda(&instance_pda, &admin_keypair.pubkey());
    info!(
        %instance_seed_pubkey,
        %instance_pda,
        operator = %admin_keypair.pubkey(),
        %operator_pda,
        "Derived PDAs for withdraw setup",
    );

    let l1_rpc = Arc::new(RpcClient::new_with_commitment(
        l1_rpc_url.to_owned(),
        CommitmentConfig::processed(),
    ));
    let send_retry_delays: &[u64] = &[1, 2, 4, 8, 16, 30];

    // ------------------------------------------------------------------
    // Task 2b: Ensure admin has SOL on L1
    // ------------------------------------------------------------------
    let balance = l1_rpc
        .get_balance(&admin_keypair.pubkey())
        .await
        .context("get_balance for admin on L1")?;
    if balance < MIN_ADMIN_LAMPORTS {
        let sig = l1_rpc
            .request_airdrop(&admin_keypair.pubkey(), AIRDROP_LAMPORTS)
            .await
            .context("airdrop to admin on L1")?;
        for _ in 0..60u32 {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if l1_rpc
                .get_balance(&admin_keypair.pubkey())
                .await
                .unwrap_or(0)
                >= AIRDROP_LAMPORTS
            {
                break;
            }
        }
        if l1_rpc
            .get_balance(&admin_keypair.pubkey())
            .await
            .unwrap_or(0)
            < MIN_ADMIN_LAMPORTS
        {
            return Err(anyhow::anyhow!(
                "airdrop timed out: admin balance still below minimum after 60 attempts"
            ));
        }
        info!(lamports = AIRDROP_LAMPORTS, sig = %sig, "Admin airdropped on L1");
    } else {
        info!(balance, "Admin already funded on L1, skipping airdrop");
    }

    // ------------------------------------------------------------------
    // Task 3: CreateInstance on L1
    // ------------------------------------------------------------------
    let t3 = Instant::now();
    let create_sig = 'send: {
        let mut last_err = String::new();
        for (attempt, &delay_secs) in send_retry_delays.iter().enumerate() {
            match l1_rpc.get_latest_blockhash().await {
                Err(e) => {
                    warn!(attempt, err = %e,
                        "get_latest_blockhash failed (create_instance), retrying in {delay_secs}s");
                    last_err = e.to_string();
                }
                Ok(blockhash) => {
                    let create_ix = CreateInstance {
                        payer: admin_keypair.pubkey(),
                        admin: admin_keypair.pubkey(),
                        instance_seed: instance_seed_pubkey,
                        instance: instance_pda,
                        system_program: program::id(),
                        event_authority,
                        contra_escrow_program: CONTRA_ESCROW_PROGRAM_ID,
                    }
                    .instruction(CreateInstanceInstructionArgs {
                        bump: instance_bump,
                    });
                    let tx = Transaction::new_signed_with_payer(
                        &[create_ix],
                        Some(&admin_keypair.pubkey()),
                        &[admin_keypair.as_ref(), &instance_seed_keypair],
                        blockhash,
                    );
                    match l1_rpc.send_transaction(&tx).await {
                        Ok(sig) => break 'send sig,
                        Err(e) => {
                            warn!(attempt, err = %e, "create_instance send failed, retrying");
                            last_err = e.to_string();
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(delay_secs)).await;
        }
        return Err(anyhow::anyhow!(
            "create_instance: all retries exhausted: {last_err}"
        ));
    };
    let retry = poll_confirmations(&l1_rpc, &[Some(create_sig)], "create_instance", 0, 1).await?;
    if !retry.is_empty() {
        return Err(anyhow::anyhow!(
            "create_instance failed to confirm on-chain"
        ));
    }
    info!(%instance_pda, elapsed_ms = t3.elapsed().as_millis(), "Escrow instance created on L1");

    // ------------------------------------------------------------------
    // Task 4: AddOperator on L1
    //
    // Register admin as the ReleaseFunds operator so operator-contra
    // (which signs with the admin key) can call ReleaseFunds on this instance.
    // ------------------------------------------------------------------
    let t4 = Instant::now();
    let add_op_sig = 'send: {
        let mut last_err = String::new();
        for (attempt, &delay_secs) in send_retry_delays.iter().enumerate() {
            match l1_rpc.get_latest_blockhash().await {
                Err(e) => {
                    warn!(attempt, err = %e,
                        "get_latest_blockhash failed (add_operator), retrying in {delay_secs}s");
                    last_err = e.to_string();
                }
                Ok(blockhash) => {
                    let add_op_ix = AddOperator {
                        payer: admin_keypair.pubkey(),
                        admin: admin_keypair.pubkey(),
                        instance: instance_pda,
                        operator: admin_keypair.pubkey(),
                        operator_pda,
                        system_program: program::id(),
                        event_authority,
                        contra_escrow_program: CONTRA_ESCROW_PROGRAM_ID,
                    }
                    .instruction(AddOperatorInstructionArgs {
                        bump: operator_bump,
                    });
                    let tx = Transaction::new_signed_with_payer(
                        &[add_op_ix],
                        Some(&admin_keypair.pubkey()),
                        &[admin_keypair.as_ref()],
                        blockhash,
                    );
                    match l1_rpc.send_transaction(&tx).await {
                        Ok(sig) => break 'send sig,
                        Err(e) => {
                            warn!(attempt, err = %e, "add_operator send failed, retrying");
                            last_err = e.to_string();
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(delay_secs)).await;
        }
        return Err(anyhow::anyhow!(
            "add_operator: all retries exhausted: {last_err}"
        ));
    };
    let retry = poll_confirmations(&l1_rpc, &[Some(add_op_sig)], "add_operator", 0, 1).await?;
    if !retry.is_empty() {
        return Err(anyhow::anyhow!("add_operator failed to confirm on-chain"));
    }
    info!(%operator_pda, elapsed_ms = t4.elapsed().as_millis(), "Operator registered on L1");

    // ------------------------------------------------------------------
    // Task 5: Create L1 SPL mint
    //
    // A single mint keypair is generated here and reused for L2 (Task 7)
    // so both chains share the same mint pubkey — required for ReleaseFunds
    // to use the correct L1 token_program when deriving ATAs.
    // ------------------------------------------------------------------
    let t5 = Instant::now();
    let mint_keypair = Arc::new(Keypair::new());
    let mint = mint_keypair.pubkey();
    let mint_rent = l1_rpc
        .get_minimum_balance_for_rent_exemption(SplMint::LEN)
        .await
        .context("get_minimum_balance_for_rent_exemption (mint)")?;
    let mint_sig = 'send: {
        let mut last_err = String::new();
        for (attempt, &delay_secs) in send_retry_delays.iter().enumerate() {
            match l1_rpc.get_latest_blockhash().await {
                Err(e) => {
                    warn!(attempt, err = %e,
                        "get_latest_blockhash failed (l1 mint init), retrying in {delay_secs}s");
                    last_err = e.to_string();
                }
                Ok(blockhash) => {
                    let create_account_ix = system_instruction::create_account(
                        &admin_keypair.pubkey(),
                        &mint,
                        mint_rent,
                        SplMint::LEN as u64,
                        &spl_token::id(),
                    );
                    let init_mint_ix = spl_token::instruction::initialize_mint(
                        &spl_token::id(),
                        &mint,
                        &admin_keypair.pubkey(),
                        None,
                        MINT_DECIMALS,
                    )
                    .unwrap();
                    let tx = Transaction::new_signed_with_payer(
                        &[create_account_ix, init_mint_ix],
                        Some(&admin_keypair.pubkey()),
                        &[admin_keypair.as_ref(), mint_keypair.as_ref()],
                        blockhash,
                    );
                    match l1_rpc.send_transaction(&tx).await {
                        Ok(sig) => break 'send sig,
                        Err(e) => {
                            warn!(attempt, err = %e, "l1 mint init send failed, retrying");
                            last_err = e.to_string();
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(delay_secs)).await;
        }
        return Err(anyhow::anyhow!(
            "l1 mint init: all retries exhausted: {last_err}"
        ));
    };
    let retry = poll_confirmations(&l1_rpc, &[Some(mint_sig)], "l1_mint_init", 0, 1).await?;
    if !retry.is_empty() {
        return Err(anyhow::anyhow!("l1_mint_init failed to confirm on-chain"));
    }
    info!(%mint, elapsed_ms = t5.elapsed().as_millis(), "L1 mint initialized");

    // ------------------------------------------------------------------
    // Task 6: AllowMint — register mint with the escrow instance
    //
    // Creates the allowed_mint PDA and the instance ATA on L1.
    // ------------------------------------------------------------------
    let t6 = Instant::now();
    let (allowed_mint_pda, allow_bump) = find_allowed_mint_pda(&instance_pda, &mint);
    let instance_ata = get_associated_token_address(&instance_pda, &mint);
    let allow_sig = 'send: {
        let mut last_err = String::new();
        for (attempt, &delay_secs) in send_retry_delays.iter().enumerate() {
            match l1_rpc.get_latest_blockhash().await {
                Err(e) => {
                    warn!(attempt, err = %e,
                        "get_latest_blockhash failed (allow_mint), retrying in {delay_secs}s");
                    last_err = e.to_string();
                }
                Ok(blockhash) => {
                    let allow_ix = AllowMint {
                        payer: admin_keypair.pubkey(),
                        admin: admin_keypair.pubkey(),
                        instance: instance_pda,
                        mint,
                        allowed_mint: allowed_mint_pda,
                        instance_ata,
                        system_program: program::id(),
                        token_program: spl_token::id(),
                        associated_token_program: spl_associated_token_account::id(),
                        event_authority,
                        contra_escrow_program: CONTRA_ESCROW_PROGRAM_ID,
                    }
                    .instruction(AllowMintInstructionArgs { bump: allow_bump });
                    let tx = Transaction::new_signed_with_payer(
                        &[allow_ix],
                        Some(&admin_keypair.pubkey()),
                        &[admin_keypair.as_ref()],
                        blockhash,
                    );
                    match l1_rpc.send_transaction(&tx).await {
                        Ok(sig) => break 'send sig,
                        Err(e) => {
                            warn!(attempt, err = %e, "allow_mint send failed, retrying");
                            last_err = e.to_string();
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(delay_secs)).await;
        }
        return Err(anyhow::anyhow!(
            "allow_mint: all retries exhausted: {last_err}"
        ));
    };
    let retry = poll_confirmations(&l1_rpc, &[Some(allow_sig)], "allow_mint", 0, 1).await?;
    if !retry.is_empty() {
        return Err(anyhow::anyhow!("allow_mint failed to confirm on-chain"));
    }
    info!(
        %allowed_mint_pda,
        %instance_ata,
        elapsed_ms = t6.elapsed().as_millis(),
        "AllowMint confirmed — instance ATA created on L1",
    );

    // ------------------------------------------------------------------
    // Task 7: Seed instance ATA with tokens on L1
    //
    // Mint num_accounts × initial_balance tokens directly to the instance ATA.
    // This is the pool that ReleaseFunds draws from — no real deposits needed.
    // ------------------------------------------------------------------
    let t7 = Instant::now();
    let seed_amount = (num_accounts as u64).saturating_mul(initial_balance);
    let seed_sig = 'send: {
        let mut last_err = String::new();
        for (attempt, &delay_secs) in send_retry_delays.iter().enumerate() {
            match l1_rpc.get_latest_blockhash().await {
                Err(e) => {
                    warn!(attempt, err = %e,
                        "get_latest_blockhash failed (seed_instance_ata), retrying in {delay_secs}s");
                    last_err = e.to_string();
                }
                Ok(blockhash) => {
                    let tx = create_admin_mint_to(
                        &admin_keypair,
                        &mint,
                        &instance_ata,
                        seed_amount,
                        blockhash,
                    );
                    match l1_rpc
                        .send_transaction_with_config(
                            &tx,
                            RpcSendTransactionConfig {
                                skip_preflight: true,
                                ..Default::default()
                            },
                        )
                        .await
                    {
                        Ok(sig) => break 'send sig,
                        Err(e) => {
                            warn!(attempt, err = %e, "seed_instance_ata send failed, retrying");
                            last_err = e.to_string();
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(delay_secs)).await;
        }
        return Err(anyhow::anyhow!(
            "seed_instance_ata: all retries exhausted: {last_err}"
        ));
    };
    let retry = poll_confirmations(&l1_rpc, &[Some(seed_sig)], "seed_instance_ata", 0, 1).await?;
    if !retry.is_empty() {
        return Err(anyhow::anyhow!(
            "seed_instance_ata failed to confirm on-chain"
        ));
    }
    info!(
        %instance_ata,
        seed_amount,
        elapsed_ms = t7.elapsed().as_millis(),
        "Instance ATA seeded on L1",
    );

    // ------------------------------------------------------------------
    // Task 8: Generate N withdrawer keypairs
    //
    // Generated here (before L2 phase) so that L1 ATAs can be created
    // for the same pubkeys that will be used as ReleaseFunds recipients.
    // ------------------------------------------------------------------
    let keypairs: Vec<Arc<Keypair>> = (0..num_accounts)
        .into_par_iter()
        .map(|_| Arc::new(Keypair::new()))
        .collect();
    info!(count = keypairs.len(), "Generated withdrawer keypairs");

    // ------------------------------------------------------------------
    // Task 8b: Create L1 ATAs for all withdrawer keypairs
    //
    // ReleaseFunds calls validate_ata() on the recipient's L1 ATA, which
    // returns InvalidInstructionData if the account is empty (doesn't
    // exist). Create all recipient ATAs on L1 before any withdrawals
    // can be released.
    // ------------------------------------------------------------------
    {
        let t8b = Instant::now();
        let total = keypairs.len();
        let mut to_send: Vec<Arc<Keypair>> = keypairs.clone();
        let mut batch_num = 0usize;
        let mut confirmed_so_far = 0usize;
        while !to_send.is_empty() {
            let mut next_round: Vec<Arc<Keypair>> = Vec::new();
            for batch in to_send.chunks(SETUP_BATCH_SIZE) {
                batch_num += 1;
                let blockhash = l1_rpc
                    .get_latest_blockhash()
                    .await
                    .context("get_latest_blockhash (l1 create-ata)")?;
                info!(
                    batch = batch_num,
                    size = batch.len(),
                    total,
                    "Creating L1 withdrawer ATA batch"
                );
                let sigs = send_parallel(
                    l1_rpc_url,
                    batch,
                    blockhash,
                    "create-l1-ata(withdraw)",
                    |kp, url, bh| {
                        let admin = Arc::clone(&admin_keypair);
                        let owner = kp.pubkey();
                        let m = mint;
                        async move {
                            let tx = create_ata_transaction(&admin, &owner, &m, bh);
                            RpcClient::new(url)
                                .send_transaction_with_config(
                                    &tx,
                                    RpcSendTransactionConfig {
                                        skip_preflight: true,
                                        ..Default::default()
                                    },
                                )
                                .await
                        }
                    },
                )
                .await;
                let retry_indices = poll_confirmations(
                    &l1_rpc,
                    &sigs,
                    "create-l1-ata(withdraw)",
                    confirmed_so_far,
                    total,
                )
                .await?;
                confirmed_so_far += batch.len() - retry_indices.len();
                for i in retry_indices {
                    next_round.push(Arc::clone(&batch[i]));
                }
            }
            to_send = next_round;
            if !to_send.is_empty() {
                warn!(count = to_send.len(), "Retrying failed L1 ATA transactions");
            }
        }
        info!(
            total,
            elapsed_ms = t8b.elapsed().as_millis(),
            "All L1 withdrawer ATAs created"
        );
    }

    // ====================================================================
    // L2 phase — contra write-node
    // ====================================================================

    let t_l2 = Instant::now();
    info!("Starting L2 setup phase");

    let l2_rpc = RpcClient::new(l2_rpc_url.to_owned());

    // ------------------------------------------------------------------
    // Task 9: Initialize same mint on L2
    //
    // The L2 write-node creates accounts implicitly (gasless), so only
    // `initialize_mint` is needed — no `create_account` like on L1.
    // Using the same mint pubkey ensures ReleaseFunds on L1 looks up the
    // correct token_program (spl_token) via the existing L1 mint account.
    // ------------------------------------------------------------------
    let t9 = Instant::now();
    let l2_mint_sig = 'send: {
        let mut last_err = String::new();
        for (attempt, &delay_secs) in send_retry_delays.iter().enumerate() {
            match l2_rpc.get_latest_blockhash().await {
                Err(e) => {
                    warn!(attempt, err = %e,
                        "get_latest_blockhash failed (l2 mint init), retrying in {delay_secs}s");
                    last_err = e.to_string();
                }
                Ok(blockhash) => {
                    let init_tx = create_admin_initialize_mint(
                        &admin_keypair,
                        &mint,
                        MINT_DECIMALS,
                        blockhash,
                    );
                    match l2_rpc.send_transaction(&init_tx).await {
                        Ok(sig) => break 'send sig,
                        Err(e) => {
                            warn!(attempt, err = %e, "l2 mint init send failed, retrying");
                            last_err = e.to_string();
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(delay_secs)).await;
        }
        return Err(anyhow::anyhow!(
            "l2 mint init: all retries exhausted: {last_err}"
        ));
    };
    let retry = poll_confirmations(&l2_rpc, &[Some(l2_mint_sig)], "l2_mint_init", 0, 1).await?;
    if !retry.is_empty() {
        return Err(anyhow::anyhow!("l2_mint_init failed to confirm on-chain"));
    }
    info!(%mint, elapsed_ms = t9.elapsed().as_millis(), "Mint initialized on L2");

    // ------------------------------------------------------------------
    // Tasks 10 + 11: Create L2 ATAs and mint tokens in batches
    // ------------------------------------------------------------------
    let total = keypairs.len();

    // ATAs
    {
        let mut to_send: Vec<Arc<Keypair>> = keypairs.clone();
        let mut batch_num = 0usize;
        let mut confirmed_so_far = 0usize;
        while !to_send.is_empty() {
            let mut next_round: Vec<Arc<Keypair>> = Vec::new();
            for batch in to_send.chunks(SETUP_BATCH_SIZE) {
                batch_num += 1;
                let blockhash = l2_rpc
                    .get_latest_blockhash()
                    .await
                    .context("get_latest_blockhash (l2 create-ata)")?;
                info!(
                    batch = batch_num,
                    size = batch.len(),
                    total,
                    "Sending L2 ATA batch"
                );
                let sigs = send_parallel(
                    l2_rpc_url,
                    batch,
                    blockhash,
                    "create-ata(withdraw)",
                    |kp, _url, bh| {
                        let admin = Arc::clone(&admin_keypair);
                        let rpc = RpcClient::new(_url);
                        let owner = kp.pubkey();
                        let m = mint;
                        async move {
                            let tx = create_ata_transaction(&admin, &owner, &m, bh);
                            rpc.send_transaction(&tx).await
                        }
                    },
                )
                .await;
                let retry_indices = poll_confirmations(
                    &l2_rpc,
                    &sigs,
                    "create-ata(withdraw)",
                    confirmed_so_far,
                    total,
                )
                .await?;
                confirmed_so_far += batch.len() - retry_indices.len();
                for i in retry_indices {
                    next_round.push(Arc::clone(&batch[i]));
                }
            }
            to_send = next_round;
            if !to_send.is_empty() {
                warn!(count = to_send.len(), "Retrying failed L2 ATA transactions");
            }
        }
    }
    info!(total, "All L2 withdrawer ATAs confirmed");

    // Mint-to
    {
        let mut to_send: Vec<Arc<Keypair>> = keypairs.clone();
        let mut batch_num = 0usize;
        let mut confirmed_so_far = 0usize;
        while !to_send.is_empty() {
            let mut next_round: Vec<Arc<Keypair>> = Vec::new();
            for batch in to_send.chunks(SETUP_BATCH_SIZE) {
                batch_num += 1;
                let blockhash = l2_rpc
                    .get_latest_blockhash()
                    .await
                    .context("get_latest_blockhash (l2 mint-to)")?;
                info!(
                    batch = batch_num,
                    size = batch.len(),
                    total,
                    "Sending L2 mint-to batch"
                );
                let sigs = send_parallel(
                    l2_rpc_url,
                    batch,
                    blockhash,
                    "mint-to(withdraw)",
                    |kp, _url, bh| {
                        let admin = Arc::clone(&admin_keypair);
                        let ata = get_associated_token_address(&kp.pubkey(), &mint);
                        async move {
                            let tx = create_admin_mint_to(&admin, &mint, &ata, initial_balance, bh);
                            RpcClient::new(_url)
                                .send_transaction_with_config(
                                    &tx,
                                    RpcSendTransactionConfig {
                                        skip_preflight: true,
                                        ..Default::default()
                                    },
                                )
                                .await
                        }
                    },
                )
                .await;
                let retry_indices = poll_confirmations(
                    &l2_rpc,
                    &sigs,
                    "mint-to(withdraw)",
                    confirmed_so_far,
                    total,
                )
                .await?;
                confirmed_so_far += batch.len() - retry_indices.len();
                for i in retry_indices {
                    next_round.push(Arc::clone(&batch[i]));
                }
            }
            to_send = next_round;
            if !to_send.is_empty() {
                warn!(
                    count = to_send.len(),
                    "Retrying failed L2 mint-to transactions"
                );
            }
        }
    }
    info!(total, "All L2 mint-to confirmed");

    // ------------------------------------------------------------------
    // Task 12: Seed BenchState with current L2 blockhash
    // ------------------------------------------------------------------
    let initial_blockhash = l2_rpc
        .get_latest_blockhash()
        .await
        .context("get_latest_blockhash (l2 seed)")?;
    let state = Arc::new(BenchState {
        current_blockhash: RwLock::new(initial_blockhash),
    });
    info!(
        blockhash = %initial_blockhash,
        l2_elapsed_ms = t_l2.elapsed().as_millis(),
        "L2 blockhash seeded — withdraw setup complete",
    );

    Ok(WithdrawConfig {
        mint,
        keypairs,
        state,
    })
}
