use {
    anyhow::Result,
    clap::Parser,
    private_channel_core::client::{
        create_admin_initialize_mint, create_admin_mint_to, create_ata_transaction,
        create_spl_transfer, create_withdraw_funds, load_keypair,
    },
    rand::{rngs::StdRng, Rng, SeedableRng},
    solana_client::nonblocking::rpc_client::RpcClient,
    solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer},
    spl_associated_token_account::get_associated_token_address,
    std::{path::PathBuf, sync::Arc, time::Duration},
    tokio::{sync::RwLock, time},
    tracing::{error, info, warn},
};

#[derive(Parser, Debug)]
#[command(
    name = "private-channel-activity",
    about = "Generate continuous activity on PrivateChannel with token operations"
)]
struct Args {
    /// Write URL for the deployment (e.g., http://localhost:8899)
    #[arg(long)]
    rpc_url: String,

    /// Path to admin keypair
    #[arg(long)]
    admin_keypair: PathBuf,

    /// Number of users to simulate
    #[arg(short = 'u', long, default_value = "10")]
    users: usize,

    /// Initial token balance per user
    #[arg(long, default_value = "10000")]
    initial_balance: u64,

    /// Delay between user operations in milliseconds
    #[arg(long, default_value = "1000")]
    user_delay_ms: u64,

    /// Delay between admin minting rounds in milliseconds
    #[arg(long, default_value = "10000")]
    admin_delay_ms: u64,

    /// Amount to mint to each user per admin round
    #[arg(long, default_value = "100")]
    mint_amount: u64,

    /// Enable verbose output
    #[arg(short = 'v', long)]
    verbose: bool,
}

// Constants
const MINT_DECIMALS: u8 = 3;

// User action probabilities
const TRANSFER_PROBABILITY: f64 = 0.4;
const WITHDRAW_PROBABILITY: f64 = 0.2;
// Remaining probability (0.4) is for doing nothing

struct UserState {
    keypair: Arc<Keypair>,
    balance: Arc<RwLock<u64>>,
}

impl Clone for UserState {
    fn clone(&self) -> Self {
        Self {
            keypair: self.keypair.clone(),
            balance: self.balance.clone(),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(if args.verbose { "info" } else { "warn" })
        .init();

    info!("Starting activity generator with {} users", args.users);
    info!("RPC URL: {}", args.rpc_url);
    let rpc_client = RpcClient::new(args.rpc_url.clone());

    // Load admin keypair
    let admin_keypair = load_keypair(&args.admin_keypair).unwrap();
    info!("Admin keypair loaded: {}", admin_keypair.pubkey());

    // Create mint pubkey
    let mint_keypair = Keypair::new();
    let mint = mint_keypair.pubkey();
    info!("Using mint: {}", mint);

    // Initialize mint
    info!("Initializing token mint with {} decimals", MINT_DECIMALS);
    let blockhash = rpc_client.get_latest_blockhash().await?;
    let init_tx = create_admin_initialize_mint(&admin_keypair, &mint, MINT_DECIMALS, blockhash);
    match rpc_client.send_and_confirm_transaction(&init_tx).await {
        Ok(sig) => info!("Mint initialized: {}", sig),
        Err(e) => {
            error!("Failed to initialize mint: {}", e);
            return Err(e.into());
        }
    }

    // Small delay to ensure mint is created
    time::sleep(Duration::from_millis(500)).await;

    // Create user keypairs and their token accounts
    let mut users = Vec::new();
    for i in 0..args.users {
        let keypair = Keypair::new();
        info!("Creating user {}: {}", i, keypair.pubkey());

        // Create associated token account
        let blockhash = rpc_client.get_latest_blockhash().await?;
        let ata_tx = create_ata_transaction(&admin_keypair, &keypair.pubkey(), &mint, blockhash);
        match rpc_client.send_transaction(&ata_tx).await {
            Ok(sig) => info!("Created ATA for user {}: {}", i, sig),
            Err(e) => {
                warn!("Failed to create ATA for user {}: {}", i, e);
                continue;
            }
        }

        // Mint initial tokens
        let token_account = get_associated_token_address(&keypair.pubkey(), &mint);
        let blockhash = rpc_client.get_latest_blockhash().await?;
        let mint_tx = create_admin_mint_to(
            &admin_keypair,
            &mint,
            &token_account,
            args.initial_balance,
            blockhash,
        );
        match rpc_client.send_transaction(&mint_tx).await {
            Ok(sig) => info!(
                "Minted {} tokens to user {}: {}",
                args.initial_balance, i, sig
            ),
            Err(e) => {
                warn!("Failed to mint initial tokens to user {}: {}", i, e);
                continue;
            }
        }

        users.push(UserState {
            keypair: Arc::new(keypair),
            balance: Arc::new(RwLock::new(args.initial_balance)),
        });
    }

    info!("Created {} users with initial balances", users.len());

    // Clone shared state for tasks
    // Spawn user activity tasks
    let mut user_tasks = Vec::new();
    for (i, user) in users.iter().enumerate() {
        let user = user.clone();
        let users = users.clone();
        let rpc_client = RpcClient::new(args.rpc_url.clone());
        let delay_ms = args.user_delay_ms;

        let task = tokio::spawn(async move {
            run_user_activity(i, user, users, &rpc_client, mint, delay_ms).await;
        });
        user_tasks.push(task);
    }

    // Spawn admin minting task
    let admin_task = {
        let users = users.clone();
        let rpc_client = RpcClient::new(args.rpc_url.clone());
        let admin_keypair = load_keypair(&args.admin_keypair).unwrap();
        let mint_amount = args.mint_amount;
        let delay_ms = args.admin_delay_ms;

        tokio::spawn(async move {
            run_admin_minting(
                users,
                &rpc_client,
                mint,
                admin_keypair,
                mint_amount,
                delay_ms,
            )
            .await;
        })
    };

    // Wait for all tasks (they run forever unless cancelled)
    tokio::signal::ctrl_c().await?;
    info!("Shutting down activity generator...");

    // Cancel all tasks
    for task in user_tasks {
        task.abort();
    }
    admin_task.abort();

    Ok(())
}

/// Run continuous activity for a single user
async fn run_user_activity(
    user_id: usize,
    user: UserState,
    all_users: Vec<UserState>,
    rpc_client: &RpcClient,
    mint: Pubkey,
    delay_ms: u64,
) {
    let mut rng = StdRng::from_entropy();
    let token_account = get_associated_token_address(&user.keypair.pubkey(), &mint);

    loop {
        time::sleep(Duration::from_millis(delay_ms)).await;

        // Get current balance
        let balance = match rpc_client.get_token_account_balance(&token_account).await {
            Ok(b) => {
                let amount = match b.amount.parse::<u64>() {
                    Ok(amount) => amount,
                    Err(_) => {
                        warn!("Failed to parse token account balance for user {}", user_id);
                        0
                    }
                };
                *user.balance.write().await = amount;
                amount
            }
            Err(e) => {
                warn!("User {} failed to get balance: {}", user_id, e);
                continue;
            }
        };

        if balance == 0 {
            info!("User {} has zero balance, skipping", user_id);
            continue;
        }

        // Decide action based on probability
        let action_roll: f64 = rng.gen();

        if action_roll < TRANSFER_PROBABILITY {
            // Transfer tokens to another user
            if all_users.len() > 1 {
                // Pick a random recipient (not self)
                let mut recipient_idx = rng.gen_range(0, all_users.len());
                while recipient_idx == user_id {
                    recipient_idx = rng.gen_range(0, all_users.len());
                }

                let recipient = &all_users[recipient_idx];
                let amount = rng.gen_range(1, balance.min(100) + 1); // Transfer up to 100 or balance

                let blockhash = match rpc_client.get_latest_blockhash().await {
                    Ok(b) => b,
                    Err(e) => {
                        warn!("User {} failed to get blockhash: {}", user_id, e);
                        continue;
                    }
                };

                let tx = create_spl_transfer(
                    &user.keypair,
                    &recipient.keypair.pubkey(),
                    &mint,
                    amount,
                    blockhash,
                );

                match rpc_client.send_transaction(&tx).await {
                    Ok(sig) => {
                        info!(
                            "User {} transferred {} tokens to user {}: {}",
                            user_id, amount, recipient_idx, sig
                        );
                        *user.balance.write().await = balance - amount;
                        *recipient.balance.write().await += amount;
                    }
                    Err(e) => {
                        warn!("User {} transfer failed: {}", user_id, e);
                        continue;
                    }
                }
            }
        } else if action_roll < TRANSFER_PROBABILITY + WITHDRAW_PROBABILITY {
            // Withdraw tokens (burns and logs the event)
            let amount = rng.gen_range(1, balance.min(50) + 1); // Withdraw up to 50 or balance

            let blockhash = match rpc_client.get_latest_blockhash().await {
                Ok(b) => b,
                Err(e) => {
                    warn!("User {} failed to get blockhash: {}", user_id, e);
                    continue;
                }
            };

            let tx = create_withdraw_funds(&user.keypair, &mint, amount, blockhash);

            match rpc_client.send_transaction(&tx).await {
                Ok(sig) => {
                    info!("User {} withdrew {} tokens: {}", user_id, amount, sig);
                    *user.balance.write().await = balance - amount;
                }
                Err(e) => {
                    warn!("User {} withdraw failed: {}", user_id, e);
                    continue;
                }
            }
        } else {
            // Do nothing
            info!("User {} idle (balance: {})", user_id, balance);
        }
    }
}

/// Run admin minting loop
async fn run_admin_minting(
    users: Vec<UserState>,
    rpc_client: &RpcClient,
    mint: Pubkey,
    admin_keypair: Keypair,
    mint_amount: u64,
    delay_ms: u64,
) {
    loop {
        time::sleep(Duration::from_millis(delay_ms)).await;

        info!("Admin minting {} tokens to all users", mint_amount);

        for (i, user) in users.iter().enumerate() {
            let token_account = get_associated_token_address(&user.keypair.pubkey(), &mint);

            let blockhash = match rpc_client.get_latest_blockhash().await {
                Ok(b) => b,
                Err(e) => {
                    warn!("Admin failed to get blockhash: {}", e);
                    continue;
                }
            };

            let tx = create_admin_mint_to(
                &admin_keypair,
                &mint,
                &token_account,
                mint_amount,
                blockhash,
            );

            match rpc_client.send_transaction(&tx).await {
                Ok(sig) => {
                    info!("Admin minted {} tokens to user {}: {}", mint_amount, i, sig);
                    *user.balance.write().await += mint_amount;
                }
                Err(e) => {
                    warn!("Admin failed to mint to user {}: {}", i, e);
                }
            }
        }
    }
}
