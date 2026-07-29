//! Move the mint and freeze authority of channel mints from one key to another.
//!
//! SPL Token only accepts `SetAuthority` signed by the *current* authority, so a
//! new admin cannot migrate mints to itself. Every send here is signed by the old
//! key, which is why this is a one-shot operation driven by
//! `bin/migrate_mint_authority.rs` rather than anything the operator does on its
//! own: the old key exists for the length of one run and is never configured into
//! a long-lived process.
//!
//! Driven by `docs/runbooks/admin_rotation_runbook.md` § 4-5.

use crate::operator::escrow_sweep::{
    describe_authority, fetch_channel_mint_state, ChannelMintState,
};
use crate::operator::utils::rpc_util::RpcClientWithRetry;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signature, Signer};
use solana_sdk::transaction::Transaction;
use spl_token::instruction::AuthorityType;
use tracing::{info, warn};

/// Migration reads at `confirmed`, not `finalized`. It has to see the mint it just
/// wrote, and finalized lags far enough behind that the verify pass would read the
/// pre-migration state and report a false failure. The operator's own finalized
/// authority check (rotation runbook § 8) is the durable gate; this is the
/// act-on-current-state read.
const READ_COMMITMENT: CommitmentConfig = CommitmentConfig::confirmed();

/// What one mint needs. Decided for every mint before anything is sent, so a run
/// that has to abort leaves no partial work behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// Not created on the channel yet. The JIT path will create it under the
    /// current admin on the next deposit, so there is nothing to move.
    Skip,
    /// Already on the new authority, from an earlier run.
    AlreadyMigrated,
    /// Mint authority is the old key: migrate. `freeze` is false when the mint
    /// carries no freeze authority, in which case only the mint authority moves.
    Migrate { freeze: bool },
    /// Held by neither key. Migrating would need a signature we do not have, and
    /// guessing is worse than stopping.
    Foreign {
        mint_authority: Option<Pubkey>,
        freeze_authority: Option<Pubkey>,
    },
}

/// Classify one mint from the state it currently carries on the channel.
///
/// The freeze authority is allowed to be absent: `SetAuthority` cannot move an
/// authority that does not exist, and a mint nobody can freeze is not a problem to
/// fix. This matches `find_stale_authority` in the reconciliation halt, which also
/// accepts an absent freeze authority.
pub fn plan_for(state: &ChannelMintState, old: &Pubkey, new: &Pubkey) -> Plan {
    if !state.initialized {
        return Plan::Skip;
    }

    let mint_is = |key: &Pubkey| state.mint_authority == Some(*key);
    let freeze_is_or_absent = |key: &Pubkey| match state.freeze_authority {
        None => true,
        Some(actual) => actual == *key,
    };

    if mint_is(new) && freeze_is_or_absent(new) {
        return Plan::AlreadyMigrated;
    }
    if mint_is(old) && freeze_is_or_absent(old) {
        return Plan::Migrate {
            freeze: state.freeze_authority == Some(*old),
        };
    }
    Plan::Foreign {
        mint_authority: state.mint_authority,
        freeze_authority: state.freeze_authority,
    }
}

/// A mint held by a key that is neither the old nor the new authority. Collected
/// across the whole set so the caller can report every one before aborting.
#[derive(Debug, Clone)]
pub struct ForeignMint {
    pub mint: Pubkey,
    pub mint_authority: String,
    pub freeze_authority: String,
}

#[derive(Debug)]
pub enum MigrationError {
    /// A channel read failed. Nothing was sent.
    Read { mint: Pubkey, reason: String },
    /// At least one mint is held by an unknown key. Nothing was sent.
    ForeignMints(Vec<ForeignMint>),
    /// Sends were attempted; these mints do not name the new authority afterwards.
    Unverified(Vec<Pubkey>),
}

impl std::fmt::Display for MigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read { mint, reason } => {
                write!(f, "failed to read channel mint {mint}: {reason}")
            }
            Self::ForeignMints(foreign) => write!(
                f,
                "{} mint(s) are held by neither the old nor the new authority; \
                 resolve them before migrating (nothing was sent)",
                foreign.len()
            ),
            Self::Unverified(mints) => write!(
                f,
                "{} mint(s) still do not name the new authority; re-run before \
                 completing the rotation",
                mints.len()
            ),
        }
    }
}

impl std::error::Error for MigrationError {}

/// Per-mint result, in the order the mints were supplied.
#[derive(Debug, Clone)]
pub struct MintReport {
    pub mint: Pubkey,
    pub plan: Plan,
    /// `Some` once a send landed, `None` for skipped and dry-run mints.
    pub signature: Option<Signature>,
    /// `Some` when the send failed. The mint keeps its old authority.
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MigrationOutcome {
    pub reports: Vec<MintReport>,
    pub dry_run: bool,
}

impl MigrationOutcome {
    pub fn migrated(&self) -> usize {
        self.reports
            .iter()
            .filter(|r| r.signature.is_some())
            .count()
    }

    pub fn failed(&self) -> Vec<&MintReport> {
        self.reports.iter().filter(|r| r.error.is_some()).collect()
    }
}

/// Move both authorities of one mint in a single transaction, so a mint is never
/// left with the two split across keys.
///
/// `freeze` is false for a mint with no freeze authority; including a
/// `FreezeAccount` `SetAuthority` for it would fail the whole transaction and
/// leave the mint authority behind too.
async fn migrate_one(
    channel_rpc: &RpcClientWithRetry,
    mint: &Pubkey,
    old_authority: &Keypair,
    new_authority: &Pubkey,
    freeze: bool,
) -> Result<Signature, Box<dyn std::error::Error>> {
    let mut authority_types = vec![AuthorityType::MintTokens];
    if freeze {
        authority_types.push(AuthorityType::FreezeAccount);
    }

    let mut instructions = Vec::with_capacity(authority_types.len());
    for authority_type in authority_types {
        instructions.push(spl_token::instruction::set_authority(
            &spl_token::id(),
            mint,
            Some(new_authority),
            authority_type,
            &old_authority.pubkey(),
            &[],
        )?);
    }

    let blockhash = channel_rpc.rpc_client.get_latest_blockhash().await?;
    let transaction = Transaction::new_signed_with_payer(
        &instructions,
        Some(&old_authority.pubkey()),
        &[old_authority],
        blockhash,
    );

    // `send_and_confirm_transaction` rather than the operator's
    // `sign_and_send_transaction` + `check_transaction_status` pair: this is a
    // one-shot human-supervised command with no retry state machine to feed, and
    // the verify pass below re-reads every mint, so a false "send failed" on a
    // transaction that actually landed self-corrects on the next run.
    Ok(channel_rpc
        .rpc_client
        .send_and_confirm_transaction(&transaction)
        .await?)
}

/// Classify every mint, then migrate the ones held by the old key.
///
/// Aborts with `ForeignMints` before sending anything if any mint is held by an
/// unknown key: a partially-rotated set is worse than an unstarted one. With
/// `dry_run` the classification is returned and nothing is sent.
///
/// After sending, re-reads every mint and returns `Unverified` if any still fails
/// to name the new authority, so a caller's exit code reflects channel state
/// rather than what the sends reported.
pub async fn migrate_mint_authorities(
    channel_rpc: &RpcClientWithRetry,
    mints: &[Pubkey],
    old_authority: &Keypair,
    new_authority: &Pubkey,
    dry_run: bool,
) -> Result<MigrationOutcome, MigrationError> {
    let old_pubkey = old_authority.pubkey();

    let mut plans = Vec::with_capacity(mints.len());
    let mut foreign = Vec::new();
    for mint in mints {
        let state = fetch_channel_mint_state(channel_rpc, mint, READ_COMMITMENT)
            .await
            .map_err(|e| MigrationError::Read {
                mint: *mint,
                reason: e.reason,
            })?;
        let plan = plan_for(&state, &old_pubkey, new_authority);
        if let Plan::Foreign {
            mint_authority,
            freeze_authority,
        } = plan
        {
            warn!(
                mint = %mint,
                mint_authority = %describe_authority(mint_authority),
                freeze_authority = %describe_authority(freeze_authority),
                "channel mint is held by neither the old nor the new authority"
            );
            foreign.push(ForeignMint {
                mint: *mint,
                mint_authority: describe_authority(mint_authority),
                freeze_authority: describe_authority(freeze_authority),
            });
        }
        plans.push((*mint, plan));
    }

    if !foreign.is_empty() {
        return Err(MigrationError::ForeignMints(foreign));
    }

    let mut reports = Vec::with_capacity(plans.len());
    for (mint, plan) in plans {
        let mut report = MintReport {
            mint,
            plan,
            signature: None,
            error: None,
        };
        if let (Plan::Migrate { freeze }, false) = (plan, dry_run) {
            match migrate_one(channel_rpc, &mint, old_authority, new_authority, freeze).await {
                Ok(signature) => {
                    info!(mint = %mint, %signature, "migrated channel mint authority");
                    report.signature = Some(signature);
                }
                Err(e) => {
                    warn!(mint = %mint, "failed to migrate channel mint authority: {}", e);
                    report.error = Some(e.to_string());
                }
            }
        }
        reports.push(report);
    }

    if dry_run {
        return Ok(MigrationOutcome {
            reports,
            dry_run: true,
        });
    }

    let mut unverified = Vec::new();
    for mint in mints {
        let state = fetch_channel_mint_state(channel_rpc, mint, READ_COMMITMENT)
            .await
            .map_err(|e| MigrationError::Read {
                mint: *mint,
                reason: e.reason,
            })?;
        if !state.initialized {
            continue;
        }
        if plan_for(&state, &old_pubkey, new_authority) != Plan::AlreadyMigrated {
            warn!(
                mint = %mint,
                mint_authority = %describe_authority(state.mint_authority),
                freeze_authority = %describe_authority(state.freeze_authority),
                "channel mint still does not name the new authority"
            );
            unverified.push(*mint);
        }
    }

    if !unverified.is_empty() {
        return Err(MigrationError::Unverified(unverified));
    }

    Ok(MigrationOutcome {
        reports,
        dry_run: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pk(seed: u8) -> Pubkey {
        Pubkey::new_from_array([seed; 32])
    }

    fn state(mint_authority: Option<Pubkey>, freeze_authority: Option<Pubkey>) -> ChannelMintState {
        ChannelMintState {
            supply: 100,
            mint_authority,
            freeze_authority,
            initialized: true,
        }
    }

    #[test]
    fn old_authority_on_both_migrates_both() {
        let old = pk(1);
        let new = pk(2);
        assert_eq!(
            plan_for(&state(Some(old), Some(old)), &old, &new),
            Plan::Migrate { freeze: true }
        );
    }

    /// A mint with no freeze authority migrates the mint authority only. Asking
    /// SPL to move an authority that does not exist would fail the transaction and
    /// strand the mint authority too.
    #[test]
    fn absent_freeze_authority_migrates_mint_authority_only() {
        let old = pk(1);
        let new = pk(2);
        assert_eq!(
            plan_for(&state(Some(old), None), &old, &new),
            Plan::Migrate { freeze: false }
        );
    }

    #[test]
    fn new_authority_on_both_is_already_migrated() {
        let old = pk(1);
        let new = pk(2);
        assert_eq!(
            plan_for(&state(Some(new), Some(new)), &old, &new),
            Plan::AlreadyMigrated
        );
    }

    /// Re-running against a mint that had no freeze authority to begin with must
    /// report it done, not try again.
    #[test]
    fn migrated_mint_without_freeze_authority_is_already_migrated() {
        let old = pk(1);
        let new = pk(2);
        assert_eq!(
            plan_for(&state(Some(new), None), &old, &new),
            Plan::AlreadyMigrated
        );
    }

    /// Authorities split across the two keys. We cannot sign for both with either
    /// key alone, so it aborts the run rather than being half-fixed.
    #[test]
    fn split_authorities_are_foreign() {
        let old = pk(1);
        let new = pk(2);
        assert!(matches!(
            plan_for(&state(Some(old), Some(new)), &old, &new),
            Plan::Foreign { .. }
        ));
    }

    #[test]
    fn third_party_authority_is_foreign() {
        let old = pk(1);
        let new = pk(2);
        let stranger = pk(3);
        assert!(matches!(
            plan_for(&state(Some(stranger), Some(stranger)), &old, &new),
            Plan::Foreign { .. }
        ));
    }

    /// A cleared mint authority cannot be moved by anyone. Reporting it as foreign
    /// stops the run so an operator sees it instead of a silent skip.
    #[test]
    fn cleared_mint_authority_is_foreign() {
        let old = pk(1);
        let new = pk(2);
        assert!(matches!(
            plan_for(&state(None, None), &old, &new),
            Plan::Foreign { .. }
        ));
    }

    #[test]
    fn uninitialized_mint_is_skipped() {
        let old = pk(1);
        let new = pk(2);
        let uninitialized = ChannelMintState {
            supply: 0,
            mint_authority: None,
            freeze_authority: None,
            initialized: false,
        };
        assert_eq!(plan_for(&uninitialized, &old, &new), Plan::Skip);
    }
}
