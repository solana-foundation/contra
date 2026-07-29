//! CLI for moving channel mint authorities from the old admin key to a new one.
//!
//! Argument parsing, mint enumeration and reporting only. The migration itself
//! lives in `operator::mint_authority_migration` so it can be covered by an
//! integration test against a real validator.
//!
//! Driven by `docs/runbooks/admin_rotation_runbook.md` § 4-5.

use clap::Parser;
use private_channel_indexer::config::PostgresConfig;
use private_channel_indexer::operator::escrow_sweep::describe_authority;
use private_channel_indexer::operator::mint_authority_migration::{
    migrate_mint_authorities, MigrationError, Plan,
};
use private_channel_indexer::operator::utils::rpc_util::{RetryConfig, RpcClientWithRetry};
use private_channel_indexer::storage::{PostgresDb, Storage};
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{read_keypair_file, Signer};
use std::str::FromStr;

#[derive(Parser, Debug)]
#[command(
    name = "migrate-mint-authority",
    about = "Move channel mint and freeze authority from the old admin key to a new one"
)]
struct Args {
    /// Indexer database, used only to enumerate which mints exist.
    #[arg(long, env = "DATABASE_URL")]
    database_url: String,

    /// PrivateChannel RPC. The channel mints live here, not on the source chain.
    #[arg(long, env = "CHANNEL_RPC_URL")]
    channel_rpc_url: String,

    /// Keypair file for the key that currently holds the authorities. It has to
    /// sign every SetAuthority, so the migration must run before it is retired.
    #[arg(long)]
    old_authority_keypair: String,

    /// Pubkey the authorities move to. This is what the operator's ADMIN_SIGNER
    /// must resolve to once the rotation completes.
    #[arg(long)]
    new_authority: String,

    /// Classify and report without sending anything.
    #[arg(long)]
    dry_run: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    let old_authority = read_keypair_file(&args.old_authority_keypair)
        .map_err(|e| format!("Failed to read {}: {}", args.old_authority_keypair, e))?;
    let new_authority = Pubkey::from_str(&args.new_authority)?;

    if old_authority.pubkey() == new_authority {
        return Err("old and new authority are the same key; nothing to migrate".into());
    }

    println!("Old authority: {}", old_authority.pubkey());
    println!("New authority: {}", new_authority);
    println!("Channel RPC:   {}", args.channel_rpc_url);
    if args.dry_run {
        println!("Mode:          dry run, nothing will be sent");
    }

    let storage = Storage::Postgres(
        PostgresDb::new(&PostgresConfig {
            database_url: args.database_url.clone(),
            max_connections: 2,
        })
        .await?,
    );

    // Every mint the indexer has ever seen, blocked ones included: a blocked
    // mint's channel tokens still exist and its authority still needs moving.
    let mut mints = Vec::new();
    for row in storage.get_escrow_balances_by_mint().await? {
        mints.push(Pubkey::from_str(&row.mint_address)?);
    }
    mints.sort();
    println!("Mints to inspect: {}\n", mints.len());

    let channel_rpc = RpcClientWithRetry::with_retry_config(
        args.channel_rpc_url.clone(),
        RetryConfig::default(),
        CommitmentConfig::confirmed(),
    );

    let outcome = match migrate_mint_authorities(
        &channel_rpc,
        &mints,
        &old_authority,
        &new_authority,
        args.dry_run,
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(MigrationError::ForeignMints(foreign)) => {
            for entry in &foreign {
                println!(
                    "{}  FOREIGN  mint_authority={} freeze_authority={}",
                    entry.mint, entry.mint_authority, entry.freeze_authority
                );
            }
            return Err(MigrationError::ForeignMints(foreign).into());
        }
        Err(e) => return Err(e.into()),
    };

    for report in &outcome.reports {
        match report.plan {
            Plan::Skip => println!("{}  SKIP     not initialized on the channel", report.mint),
            Plan::AlreadyMigrated => {
                println!("{}  OK       already on the new authority", report.mint)
            }
            Plan::Migrate { freeze } if outcome.dry_run => {
                let scope = if freeze {
                    "mint + freeze authority"
                } else {
                    "mint authority only, no freeze authority set"
                };
                println!("{}  WOULD MIGRATE  {}", report.mint, scope);
            }
            Plan::Migrate { .. } => match (&report.signature, &report.error) {
                (Some(signature), _) => {
                    println!("{}  MIGRATED  {}", report.mint, signature)
                }
                (None, Some(error)) => {
                    println!("{}  FAILED    {}", report.mint, error)
                }
                (None, None) => println!("{}  MIGRATE   no result recorded", report.mint),
            },
            // migrate_mint_authorities aborts on foreign mints before reporting.
            Plan::Foreign {
                mint_authority,
                freeze_authority,
            } => println!(
                "{}  FOREIGN  mint_authority={} freeze_authority={}",
                report.mint,
                describe_authority(mint_authority),
                describe_authority(freeze_authority)
            ),
        }
    }

    if outcome.dry_run {
        println!("\nDry run complete, nothing was sent.");
        return Ok(());
    }

    println!("\nMigrated {} mint(s).", outcome.migrated());
    let failed = outcome.failed();
    if !failed.is_empty() {
        return Err(format!(
            "{} mint(s) failed to migrate; re-run before completing the rotation",
            failed.len()
        )
        .into());
    }

    println!("Every channel mint names the new authority.");
    Ok(())
}
