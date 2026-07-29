//! Integration test for `operator::mint_authority_migration` against a real
//! validator and the real SPL Token program.
//!
//! The unit tests cover `plan_for`, which is pure. What only a validator can show
//! is that the classification actually matches what SPL accepts: that both
//! authorities move in one transaction, that a mint with no freeze authority is
//! migrated without asking SPL to move an authority that does not exist (which
//! would fail the whole transaction and strand the mint authority too), and that
//! re-running is idempotent.
//!
//! No Postgres: the migration takes a plain mint list, so enumeration stays in the
//! CLI and is not under test here.
//!
//! Backs `docs/runbooks/admin_rotation_runbook.md` § 4-5.

#[path = "helpers/mod.rs"]
mod helpers;

use helpers::{generate_mint, send_and_confirm_instructions};
use private_channel_indexer::operator::escrow_sweep::fetch_channel_mint_state;
use private_channel_indexer::operator::mint_authority_migration::{
    migrate_mint_authorities, MigrationError, Plan,
};
use private_channel_indexer::operator::utils::rpc_util::{RetryConfig, RpcClientWithRetry};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::signature::{Keypair, Signer};
use spl_token::instruction::AuthorityType;
use test_utils::validator_helper::start_test_validator_no_geyser;

/// The migration reads at `confirmed`, so the test's assertions read at the same
/// level. `finalized` lags ~32 slots on a fresh validator, which would show mints
/// created moments ago as not yet existing.
const READ_COMMITMENT: CommitmentConfig = CommitmentConfig::confirmed();

#[tokio::test(flavor = "multi_thread")]
async fn mint_authority_migration_moves_authorities_and_is_idempotent(
) -> Result<(), Box<dyn std::error::Error>> {
    let (test_validator, faucet) = start_test_validator_no_geyser().await;
    let client =
        RpcClient::new_with_commitment(test_validator.rpc_url(), CommitmentConfig::confirmed());
    let channel_rpc = RpcClientWithRetry::with_retry_config(
        test_validator.rpc_url(),
        RetryConfig::default(),
        CommitmentConfig::confirmed(),
    );

    // The faucet is the old authority so it can pay its own fees; the new
    // authority never signs, it only has to be a pubkey.
    let old_authority = faucet;
    let new_authority = Keypair::new().pubkey();

    // Mint 1: both authorities on the old key, the shape `InitializeMintBuilder`
    // produces on the channel.
    let both = generate_mint(&client, &old_authority, &old_authority, &Keypair::new()).await?;

    // Mint 2: freeze authority cleared, so only the mint authority can move.
    let freezeless =
        generate_mint(&client, &old_authority, &old_authority, &Keypair::new()).await?;
    send_and_confirm_instructions(
        &client,
        &[spl_token::instruction::set_authority(
            &spl_token::id(),
            &freezeless,
            None,
            AuthorityType::FreezeAccount,
            &old_authority.pubkey(),
            &[],
        )?],
        &old_authority,
        &[&old_authority],
        "Clear freeze authority",
    )
    .await?;

    let mints = vec![both, freezeless];

    // ── Dry run classifies without sending ──────────────────────────────────
    let dry = migrate_mint_authorities(&channel_rpc, &mints, &old_authority, &new_authority, true)
        .await?;

    assert_eq!(
        dry.reports[0].plan,
        Plan::Migrate { freeze: true },
        "a mint holding both authorities must migrate both"
    );
    assert_eq!(
        dry.reports[1].plan,
        Plan::Migrate { freeze: false },
        "a mint with no freeze authority must migrate the mint authority only"
    );
    assert_eq!(dry.migrated(), 0, "a dry run must send nothing");
    assert_eq!(
        fetch_channel_mint_state(&channel_rpc, &both, READ_COMMITMENT)
            .await?
            .mint_authority,
        Some(old_authority.pubkey()),
        "a dry run must leave the on-chain authority untouched"
    );

    // ── Real run moves the authorities ──────────────────────────────────────
    let outcome =
        migrate_mint_authorities(&channel_rpc, &mints, &old_authority, &new_authority, false)
            .await?;

    assert_eq!(outcome.migrated(), 2);
    assert!(outcome.failed().is_empty(), "{:?}", outcome.failed());

    let both_state = fetch_channel_mint_state(&channel_rpc, &both, READ_COMMITMENT).await?;
    assert_eq!(both_state.mint_authority, Some(new_authority));
    assert_eq!(both_state.freeze_authority, Some(new_authority));

    let freezeless_state =
        fetch_channel_mint_state(&channel_rpc, &freezeless, READ_COMMITMENT).await?;
    assert_eq!(freezeless_state.mint_authority, Some(new_authority));
    assert_eq!(
        freezeless_state.freeze_authority, None,
        "migration must not invent a freeze authority the mint never had"
    );

    // ── Re-running is idempotent ────────────────────────────────────────────
    let rerun =
        migrate_mint_authorities(&channel_rpc, &mints, &old_authority, &new_authority, false)
            .await?;

    assert!(
        rerun
            .reports
            .iter()
            .all(|r| r.plan == Plan::AlreadyMigrated),
        "a second run must report every mint already migrated: {:?}",
        rerun.reports
    );
    assert_eq!(rerun.migrated(), 0, "a second run must send nothing");

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn foreign_mint_aborts_before_sending() -> Result<(), Box<dyn std::error::Error>> {
    let (test_validator, faucet) = start_test_validator_no_geyser().await;
    let client =
        RpcClient::new_with_commitment(test_validator.rpc_url(), CommitmentConfig::confirmed());
    let channel_rpc = RpcClientWithRetry::with_retry_config(
        test_validator.rpc_url(),
        RetryConfig::default(),
        CommitmentConfig::confirmed(),
    );

    let old_authority = faucet;
    let new_authority = Keypair::new().pubkey();

    // One migratable mint and one held by a third party. The migratable mint is
    // listed first, so if the abort happened after the send loop rather than
    // before it, its authority would already have moved.
    //
    // `generate_mint` only embeds the authority pubkey, so the stranger never
    // signs and needs no funding.
    let migratable =
        generate_mint(&client, &old_authority, &old_authority, &Keypair::new()).await?;
    let stranger = Keypair::new();
    let foreign = generate_mint(&client, &old_authority, &stranger, &Keypair::new()).await?;

    let result = migrate_mint_authorities(
        &channel_rpc,
        &[migratable, foreign],
        &old_authority,
        &new_authority,
        false,
    )
    .await;

    match result {
        Err(MigrationError::ForeignMints(entries)) => {
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].mint, foreign);
        }
        other => panic!("expected ForeignMints, got {other:?}"),
    }

    assert_eq!(
        fetch_channel_mint_state(&channel_rpc, &migratable, READ_COMMITMENT)
            .await?
            .mint_authority,
        Some(old_authority.pubkey()),
        "a foreign mint must abort the run before any authority moves"
    );

    Ok(())
}
