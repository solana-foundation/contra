use {
    anyhow::{anyhow, Result},
    clap::{Parser, Subcommand},
    private_channel_auth::{
        db,
        error::AppError,
        models::{Role, User},
    },
    solana_sdk::pubkey::Pubkey,
    sqlx::postgres::PgPoolOptions,
    std::{
        env,
        io::{self, Write},
        str::FromStr,
    },
    tracing::{error, info},
    uuid::Uuid,
};

#[derive(Parser, Debug)]
#[command(
    name = "private-channel-auth-admin",
    about = "Manual administrative commands for the private-channel-auth database"
)]
struct Args {
    /// Log level (trace, debug, info, warn, error)
    #[arg(long, default_value = "info", env = "PRIVATE_CHANNEL_LOG_LEVEL")]
    log_level: String,

    /// Enable JSON logging format
    #[arg(long, env = "PRIVATE_CHANNEL_JSON_LOGS")]
    json_logs: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Look up a user's immutable id, role and creation time
    ShowUser(ShowUserArgs),
    /// Attach a wallet to a user without verification — operator asserts trust
    AttachWallet(AttachWalletArgs),
    /// Set a user's RBAC role (e.g. promote to operator so the gateway grants it
    /// full account access + getTransaction)
    SetRole(SetRoleArgs),
}

#[derive(Parser, Debug)]
struct ShowUserArgs {
    /// Username to resolve to an id
    #[arg(long)]
    username: String,
}

#[derive(Parser, Debug)]
struct AttachWalletArgs {
    /// Immutable id of the user to attach the wallet to
    #[arg(long)]
    user_id: Uuid,

    /// Base58-encoded Solana pubkey to attach to the user
    #[arg(long)]
    pubkey: String,

    /// Skip the confirmation prompt
    #[arg(long)]
    yes: bool,
}

#[derive(Clone, Debug, clap::ValueEnum)]
enum RoleArg {
    Operator,
    User,
}

impl RoleArg {
    fn to_model(&self) -> Role {
        match self {
            RoleArg::Operator => Role::Operator,
            RoleArg::User => Role::User,
        }
    }
}

#[derive(Parser, Debug)]
struct SetRoleArgs {
    /// Immutable id of the user whose role to change
    #[arg(long)]
    user_id: Uuid,

    /// Role to assign
    #[arg(long)]
    role: RoleArg,

    /// Skip the confirmation prompt
    #[arg(long)]
    yes: bool,
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
    // Read AUTH_DATABASE_URL from the environment rather than a CLI flag so the
    // password never lands in argv (visible via `ps` and shell history).
    let database_url =
        env::var("AUTH_DATABASE_URL").map_err(|_| anyhow!("AUTH_DATABASE_URL is not set"))?;

    if !database_url.starts_with("postgres://") && !database_url.starts_with("postgresql://") {
        return Err(anyhow!("AUTH_DATABASE_URL must be a PostgreSQL URL"));
    }

    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .map_err(|e| anyhow!("Failed to connect to auth DB: {}", e))?;

    // Idempotent, and it keeps the tool usable against a database whose auth
    // service hasn't yet restarted onto a schema carrying `admin_audit`.
    db::init_schema(&pool).await?;

    match args.command {
        Command::ShowUser(args) => show_user(&pool, args).await?,
        Command::AttachWallet(args) => attach_wallet(&pool, &admin_actor()?, args).await?,
        Command::SetRole(args) => set_role(&pool, &admin_actor()?, args).await?,
    }

    Ok(())
}

/// Names the person running the command in the audit trail. Required rather than
/// defaulted: a row recording "unknown" is not worth writing.
fn admin_actor() -> Result<String> {
    let actor = env::var("AUTH_ADMIN_ACTOR").map_err(|_| anyhow!("AUTH_ADMIN_ACTOR is not set"))?;
    if actor.trim().is_empty() {
        return Err(anyhow!("AUTH_ADMIN_ACTOR must not be empty"));
    }
    Ok(actor)
}

fn print_user(user: &User) {
    println!("  id:         {}", user.id);
    println!("  username:   {}", user.username);
    println!("  role:       {}", user.role.as_str());
    println!("  created_at: {}", user.created_at);
}

/// Show who is about to be changed and require an explicit `yes`. These are the
/// fields an administrator matches against what the account owner reported out of
/// band, so a grant lands on the person it was meant for and not on whoever
/// registered the name first.
fn confirm(user: &User, action: &str, skip: bool) -> Result<bool> {
    if skip {
        return Ok(true);
    }

    println!("about to {action} for:");
    print_user(user);
    print!("type 'yes' to continue: ");
    io::stdout().flush()?;

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;

    Ok(answer.trim() == "yes")
}

async fn show_user(pool: &sqlx::PgPool, args: ShowUserArgs) -> Result<()> {
    let user = db::find_user_by_username(pool, &args.username)
        .await?
        .ok_or_else(|| anyhow!("user not found: {}", args.username))?;

    print_user(&user);

    Ok(())
}

async fn set_role(pool: &sqlx::PgPool, actor: &str, args: SetRoleArgs) -> Result<()> {
    let user = db::find_user_by_id(pool, args.user_id)
        .await?
        .ok_or_else(|| anyhow!("user not found: {}", args.user_id))?;

    let role = args.role.to_model();
    let role_str = role.as_str();
    if !confirm(&user, &format!("set role to {role_str}"), args.yes)? {
        println!("aborted");
        return Ok(());
    }

    // Both ends of the transition, so a real promotion is distinguishable from a
    // command that changed nothing.
    let detail = format!("{} -> {role_str}", user.role.as_str());

    let mut tx = pool.begin().await?;
    if !db::set_user_role(&mut *tx, user.id, role).await? {
        return Err(anyhow!("user not found: {}", args.user_id));
    }
    db::insert_admin_audit(&mut *tx, actor, "set_role", user.id, &detail).await?;
    tx.commit().await?;

    info!(user_id = %user.id, username = %user.username, actor, detail = %detail, "set role");
    println!("set role of {} ({}) to {role_str}", user.username, user.id);

    Ok(())
}

async fn attach_wallet(pool: &sqlx::PgPool, actor: &str, args: AttachWalletArgs) -> Result<()> {
    let pubkey = Pubkey::from_str(&args.pubkey)
        .map_err(|_| anyhow!("invalid pubkey: {}", args.pubkey))?
        .to_string();

    let user = db::find_user_by_id(pool, args.user_id)
        .await?
        .ok_or_else(|| anyhow!("user not found: {}", args.user_id))?;

    if !confirm(&user, &format!("attach wallet {pubkey}"), args.yes)? {
        println!("aborted");
        return Ok(());
    }

    let mut tx = pool.begin().await?;
    let wallet = db::insert_verified_wallet(&mut *tx, user.id, &pubkey)
        .await
        .map_err(|e| match e {
            // Unique constraint on (user_id, pubkey) — wallet already attached.
            AppError::Db(sqlx::Error::Database(ref db_err))
                if db_err.constraint() == Some("verified_wallets_user_id_pubkey_key") =>
            {
                anyhow!("wallet {} is already attached to user {}", pubkey, user.id)
            }
            other => anyhow::Error::new(other),
        })?;
    db::insert_admin_audit(&mut *tx, actor, "attach_wallet", user.id, &wallet.pubkey).await?;
    tx.commit().await?;

    info!(
        user_id = %user.id,
        username = %user.username,
        pubkey = %wallet.pubkey,
        actor,
        "attached wallet"
    );

    println!(
        "attached wallet {} to user {} ({})",
        wallet.pubkey, user.username, user.id
    );

    Ok(())
}

#[cfg(test)]
// `ENV_LOCK` is a synchronous Mutex held across `.await` on purpose: it
// serializes process-global env-var mutation across async tests. An async
// Mutex would defeat the point — the lock window must include the spawned
// child process's read of the env var, not just the in-test setup. Clippy
// can't distinguish the two cases, so silence the lint module-wide.
#[allow(clippy::await_holding_lock)]
mod tests {
    use super::*;
    use solana_sdk::pubkey::Pubkey;
    use sqlx::PgPool;
    use std::sync::Mutex;
    use testcontainers::{runners::AsyncRunner, ContainerAsync};
    use testcontainers_modules::postgres::Postgres;

    // env::set_var / remove_var mutate process-global state. The two `run` tests
    // touch AUTH_DATABASE_URL — serialize them so they don't race when the
    // surrounding test runner schedules them in parallel.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    async fn start_pool() -> (PgPool, ContainerAsync<Postgres>) {
        let container = Postgres::default()
            .with_db_name("auth_test")
            .with_user("postgres")
            .with_password("password")
            .start()
            .await
            .expect("failed to start postgres container");
        let host = container.get_host().await.unwrap();
        let port = container.get_host_port_ipv4(5432).await.unwrap();
        let url = format!("postgres://postgres:password@{}:{}/auth_test", host, port);
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .expect("failed to connect");
        db::init_schema(&pool).await.expect("schema init");
        (pool, container)
    }

    // Every test drives the commands non-interactively; the confirmation prompt
    // reads stdin, which no test has.
    fn attach_args(user_id: Uuid, pubkey: &str) -> AttachWalletArgs {
        AttachWalletArgs {
            user_id,
            pubkey: pubkey.into(),
            yes: true,
        }
    }

    fn set_role_args(user_id: Uuid, role: RoleArg) -> SetRoleArgs {
        SetRoleArgs {
            user_id,
            role,
            yes: true,
        }
    }

    async fn audit_rows(pool: &PgPool) -> Vec<(String, String, Uuid, String)> {
        sqlx::query_as(
            r#"SELECT actor, action, target_user_id, detail FROM private_channel_auth.admin_audit"#,
        )
        .fetch_all(pool)
        .await
        .expect("read audit trail")
    }

    fn run_args(command: Command) -> Args {
        Args {
            log_level: "info".into(),
            json_logs: false,
            command,
        }
    }

    #[tokio::test]
    async fn run_rejects_missing_database_url() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev = env::var("AUTH_DATABASE_URL").ok();
        env::remove_var("AUTH_DATABASE_URL");

        let result = run(run_args(Command::AttachWallet(attach_args(
            Uuid::new_v4(),
            "p",
        ))))
        .await;

        match prev {
            Some(p) => env::set_var("AUTH_DATABASE_URL", p),
            None => env::remove_var("AUTH_DATABASE_URL"),
        }

        let err = result.expect_err("expected error");
        assert!(err.to_string().contains("is not set"), "got: {err}");
    }

    #[tokio::test]
    async fn run_rejects_non_postgres_url() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev = env::var("AUTH_DATABASE_URL").ok();
        env::set_var("AUTH_DATABASE_URL", "mysql://localhost/test");

        let result = run(run_args(Command::AttachWallet(attach_args(
            Uuid::new_v4(),
            "p",
        ))))
        .await;

        match prev {
            Some(p) => env::set_var("AUTH_DATABASE_URL", p),
            None => env::remove_var("AUTH_DATABASE_URL"),
        }

        let err = result.expect_err("expected error");
        assert!(
            err.to_string().contains("must be a PostgreSQL URL"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn attach_wallet_rejects_invalid_pubkey() {
        let (pool, _c) = start_pool().await;
        let err = attach_wallet(&pool, "admin", attach_args(Uuid::new_v4(), "not-base58"))
            .await
            .expect_err("expected error");
        assert!(err.to_string().contains("invalid pubkey"), "got: {err}");
    }

    #[tokio::test]
    async fn attach_wallet_rejects_unknown_user() {
        let (pool, _c) = start_pool().await;
        let pubkey = Pubkey::new_unique().to_string();
        let err = attach_wallet(&pool, "admin", attach_args(Uuid::new_v4(), &pubkey))
            .await
            .expect_err("expected error");
        assert!(err.to_string().contains("user not found"), "got: {err}");
    }

    #[tokio::test]
    async fn attach_wallet_succeeds_for_known_user() {
        let (pool, _c) = start_pool().await;
        let user = db::insert_user(&pool, "bob", "$argon2id$placeholder")
            .await
            .expect("insert user");
        let pubkey = Pubkey::new_unique().to_string();

        attach_wallet(&pool, "admin", attach_args(user.id, &pubkey))
            .await
            .expect("attach should succeed");

        let wallets = db::list_verified_wallets(&pool, user.id)
            .await
            .expect("list wallets");
        assert_eq!(wallets.len(), 1);
        assert_eq!(wallets[0].pubkey, pubkey);

        assert_eq!(
            audit_rows(&pool).await,
            vec![(
                "admin".to_string(),
                "attach_wallet".to_string(),
                user.id,
                pubkey
            )]
        );
    }

    #[tokio::test]
    async fn attach_wallet_reports_already_attached() {
        let (pool, _c) = start_pool().await;
        let user = db::insert_user(&pool, "carol", "$argon2id$placeholder")
            .await
            .expect("insert user");
        let pubkey = Pubkey::new_unique().to_string();

        attach_wallet(&pool, "admin", attach_args(user.id, &pubkey))
            .await
            .expect("first attach should succeed");

        let err = attach_wallet(&pool, "admin", attach_args(user.id, &pubkey))
            .await
            .expect_err("second attach should fail");
        assert!(
            err.to_string().contains("is already attached"),
            "got: {err}"
        );

        // The failed second attach must not duplicate the row.
        let wallets = db::list_verified_wallets(&pool, user.id)
            .await
            .expect("list wallets");
        assert_eq!(wallets.len(), 1, "failed attach should not duplicate row");
        assert_eq!(wallets[0].pubkey, pubkey);

        // The rolled-back attach must not leave a record of a change that never happened.
        assert_eq!(audit_rows(&pool).await.len(), 1);
    }

    #[tokio::test]
    async fn set_role_promotes_user_to_operator() {
        let (pool, _c) = start_pool().await;
        let user = db::insert_user(&pool, "dave", "$argon2id$placeholder")
            .await
            .expect("insert user");

        set_role(&pool, "admin", set_role_args(user.id, RoleArg::Operator))
            .await
            .expect("set role should succeed");

        let promoted = db::find_user_by_id(&pool, user.id)
            .await
            .expect("find user")
            .expect("user exists");
        assert_eq!(promoted.role, Role::Operator);

        assert_eq!(
            audit_rows(&pool).await,
            vec![(
                "admin".to_string(),
                "set_role".to_string(),
                user.id,
                "user -> operator".to_string()
            )]
        );
    }

    #[tokio::test]
    async fn set_role_rejects_unknown_user() {
        let (pool, _c) = start_pool().await;
        let err = set_role(
            &pool,
            "admin",
            set_role_args(Uuid::new_v4(), RoleArg::Operator),
        )
        .await
        .expect_err("expected error");
        assert!(err.to_string().contains("user not found"), "got: {err}");
        assert!(audit_rows(&pool).await.is_empty());
    }
}
