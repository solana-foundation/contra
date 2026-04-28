use {
    anyhow::{anyhow, Result},
    clap::{Parser, Subcommand},
    contra_auth::db,
    solana_sdk::pubkey::Pubkey,
    sqlx::postgres::PgPoolOptions,
    std::str::FromStr,
    tracing::{error, info},
};

#[derive(Parser, Debug)]
#[command(
    name = "contra-auth-admin",
    about = "Manual administrative commands for the contra-auth database"
)]
struct Args {
    /// Auth database connection URL (PostgreSQL)
    #[arg(long, env = "AUTH_DATABASE_URL")]
    database_url: String,

    /// Log level (trace, debug, info, warn, error)
    #[arg(long, default_value = "info", env = "CONTRA_LOG_LEVEL")]
    log_level: String,

    /// Enable JSON logging format
    #[arg(long, env = "CONTRA_JSON_LOGS")]
    json_logs: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Attach a wallet to a user without verification — operator asserts trust
    AttachWallet(AttachWalletArgs),
}

#[derive(Parser, Debug)]
struct AttachWalletArgs {
    /// Username of the user to attach the wallet to
    #[arg(long)]
    username: String,

    /// Base58-encoded Solana pubkey to attach to the user
    #[arg(long)]
    pubkey: String,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    init_logging(&args.log_level, args.json_logs);

    if let Err(e) = run(args).await {
        error!("Command failed: {:?}", e);
        std::process::exit(1);
    }
}

fn init_logging(log_level: &str, json_logs: bool) {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level));

    if json_logs {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .json()
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
    }
}

async fn run(args: Args) -> Result<()> {
    if !args.database_url.starts_with("postgres://")
        && !args.database_url.starts_with("postgresql://")
    {
        return Err(anyhow!("AUTH_DATABASE_URL must be a PostgreSQL URL"));
    }

    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&args.database_url)
        .await
        .map_err(|e| anyhow!("Failed to connect to auth DB: {}", e))?;

    match args.command {
        Command::AttachWallet(args) => attach_wallet(&pool, args).await?,
    }

    Ok(())
}

async fn attach_wallet(pool: &sqlx::PgPool, args: AttachWalletArgs) -> Result<()> {
    Pubkey::from_str(&args.pubkey).map_err(|_| anyhow!("invalid pubkey: {}", args.pubkey))?;

    let user = db::find_user_by_username(pool, &args.username)
        .await?
        .ok_or_else(|| anyhow!("user not found: {}", args.username))?;

    let wallet = db::insert_verified_wallet(pool, user.id, &args.pubkey).await?;

    info!(
        user_id = %user.id,
        username = %user.username,
        pubkey = %wallet.pubkey,
        "attached wallet"
    );

    println!(
        "attached wallet {} to user {} ({})",
        wallet.pubkey, user.username, user.id
    );

    Ok(())
}
