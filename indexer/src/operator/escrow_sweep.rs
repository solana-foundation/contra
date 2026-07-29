//! Shared on-chain escrow balance sweep.
//!
//! Both the operator's continuous reconciliation and the indexer's startup
//! reconciliation need the authoritative custody view: the token balance the
//! escrow instance actually holds, summed per mint across every token account
//! it owns. Deriving the set of mints from this sweep (rather than from the DB
//! `mints` table) is what closes the startup blind spot where a fresh or
//! partially restored DB with real escrow balances would otherwise pass the
//! check without ever looking on-chain.

use crate::operator::utils::instruction_util::RetryPolicy;
use crate::operator::utils::rpc_util::RpcClientWithRetry;
use solana_account_decoder_client_types::UiAccountData;
use solana_client::rpc_request::TokenAccountsFilter;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::pubkey::Pubkey;
use spl_token::solana_program::program_pack::Pack;
use spl_token::state::Account as TokenAccount;
use spl_token::state::Mint;
use std::collections::HashMap;
use std::str::FromStr;
use tracing::warn;

/// Failure to read the escrow's on-chain token holdings. Carries a human reason
/// so each caller can wrap it in its own error type without losing context.
#[derive(Debug, Clone)]
pub struct EscrowSweepError {
    pub reason: String,
}

impl std::fmt::Display for EscrowSweepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.reason)
    }
}

impl std::error::Error for EscrowSweepError {}

/// Sum every token account owned by the escrow instance, grouped by mint, across
/// the SPL Token and Token-2022 programs. The returned map is the on-chain
/// custody snapshot; a mint absent from the map holds zero on-chain.
pub async fn fetch_escrow_balances_by_mint(
    rpc_client: &RpcClientWithRetry,
    escrow_instance_id: Pubkey,
) -> Result<HashMap<Pubkey, u64>, EscrowSweepError> {
    let mut balances = HashMap::new();
    let token_programs = [spl_token::id(), spl_token_2022::id()];

    for token_program_id in token_programs {
        let accounts = rpc_client
            .with_retry(
                "get_token_accounts_by_owner",
                RetryPolicy::Idempotent,
                || async {
                    rpc_client
                        .rpc_client
                        .get_token_accounts_by_owner_with_commitment(
                            &escrow_instance_id,
                            TokenAccountsFilter::ProgramId(token_program_id),
                            CommitmentConfig::finalized(),
                        )
                        .await
                        .map(|response| response.value)
                },
            )
            .await
            .map_err(|e| EscrowSweepError {
                reason: format!(
                    "Failed to fetch token accounts for program {token_program_id}: {e}"
                ),
            })?;

        // The RPC may return accounts in binary (base64) or JSON-parsed form
        // depending on the requested encoding; handle both.
        for keyed_account in accounts {
            let (mint, amount) = if let Some(decoded) = keyed_account.account.data.decode() {
                let token_account =
                    TokenAccount::unpack(&decoded).map_err(|e| EscrowSweepError {
                        reason: format!(
                            "Failed to parse token account for program {token_program_id}: {e}"
                        ),
                    })?;
                (token_account.mint, token_account.amount)
            } else if let UiAccountData::Json(parsed) = &keyed_account.account.data {
                let info = parsed.parsed.get("info").ok_or_else(|| EscrowSweepError {
                    reason: "Missing 'info' in parsed token account".to_string(),
                })?;
                let mint_str =
                    info.get("mint")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| EscrowSweepError {
                            reason: "Missing 'mint' in parsed token account info".to_string(),
                        })?;
                let amount_str = info
                    .get("tokenAmount")
                    .and_then(|v| v.get("amount"))
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| EscrowSweepError {
                        reason: "Missing 'tokenAmount.amount' in parsed token account".to_string(),
                    })?;
                let mint = Pubkey::from_str(mint_str).map_err(|e| EscrowSweepError {
                    reason: format!("Invalid mint pubkey '{mint_str}': {e}"),
                })?;
                let amount = amount_str.parse::<u64>().map_err(|e| EscrowSweepError {
                    reason: format!("Invalid token amount '{amount_str}': {e}"),
                })?;
                (mint, amount)
            } else {
                warn!(
                    token_program = %token_program_id,
                    "Skipping escrow token account with unrecognised data encoding"
                );
                continue;
            };

            // One mint can span several token accounts; sum them. Saturating so a corrupt
            // over-u64 sum reports u64::MAX (and trips the mismatch) instead of wrapping.
            let acc = balances.entry(mint).or_insert(0u64);
            *acc = acc.saturating_add(amount);
        }
    }

    Ok(balances)
}

/// The state of one channel mint: how much of it exists, and who controls it.
///
/// Both authority fields read `None` for an uninitialized mint and for a live
/// mint whose authority was cleared. Check `initialized` to tell those apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelMintState {
    pub supply: u64,
    pub mint_authority: Option<Pubkey>,
    pub freeze_authority: Option<Pubkey>,
    pub initialized: bool,
}

/// Render an authority for an operator-facing message. A cleared authority reads
/// as "none" rather than Debug's `None`.
pub fn describe_authority(authority: Option<Pubkey>) -> String {
    match authority {
        Some(key) => key.to_string(),
        None => "none".to_string(),
    }
}

impl ChannelMintState {
    /// The state of a channel mint that does not exist yet.
    fn absent() -> Self {
        Self {
            supply: 0,
            mint_authority: None,
            freeze_authority: None,
            initialized: false,
        }
    }
}

/// Read the channel-side state of `mint` on the PrivateChannel chain. An absent
/// mint account (nothing minted yet) reads as `ChannelMintState::absent()`; any
/// other RPC or decode failure is surfaced so a bad read never silently looks
/// like 0 supply.
///
/// `commitment` is explicit rather than taken from the client because the two
/// callers need different levels. The solvency and authority invariants read
/// `finalized`, since a halt must not fire on state that can still be rolled back.
/// The authority migration reads `confirmed`, because it has to see and act on the
/// mint it just wrote, and finalized lags too far behind for that.
pub async fn fetch_channel_mint_state(
    channel_rpc: &RpcClientWithRetry,
    mint: &Pubkey,
    commitment: CommitmentConfig,
) -> Result<ChannelMintState, EscrowSweepError> {
    // get_account_with_commitment cleanly separates a truly-absent account
    // (Ok(value = None)) from a transport/node error (Err). The plain get_account
    // convenience formats BOTH as an "AccountNotFound" error, which would let an
    // RPC outage masquerade as zero supply and blind the supply invariant.
    let response = channel_rpc
        .with_retry(
            "get_channel_mint_account",
            RetryPolicy::Idempotent,
            || async {
                channel_rpc
                    .rpc_client
                    .get_account_with_commitment(mint, commitment)
                    .await
            },
        )
        .await
        .map_err(|e| EscrowSweepError {
            reason: format!("Failed to fetch channel mint account {mint}: {e}"),
        })?;

    // Absent account = nothing minted yet.
    let account = match response.value {
        Some(account) => account,
        None => return Ok(ChannelMintState::absent()),
    };

    // The channel program mints classic SPL tokens (not Token-2022).
    // `unpack_unchecked` skips only the is_initialized check, so a wrong length
    // or an invalid COption discriminant still errors here. Inspecting
    // is_initialized ourselves keeps "allocated but not a mint" out of the
    // corruption bucket.
    let mint_state = Mint::unpack_unchecked(&account.data).map_err(|e| EscrowSweepError {
        reason: format!("Failed to parse channel mint account {mint}: {e}"),
    })?;

    if !mint_state.is_initialized {
        return Ok(ChannelMintState::absent());
    }

    Ok(ChannelMintState {
        supply: mint_state.supply,
        mint_authority: mint_state.mint_authority.into(),
        freeze_authority: mint_state.freeze_authority.into(),
        initialized: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operator::RetryConfig;
    use base64::Engine as _;
    use solana_sdk::commitment_config::CommitmentConfig;
    use spl_token::solana_program::program_option::COption;
    use spl_token::state::AccountState;

    fn client(url: &str) -> RpcClientWithRetry {
        RpcClientWithRetry::with_retry_config(
            url.to_string(),
            RetryConfig::default(),
            CommitmentConfig::finalized(),
        )
    }

    /// One `RpcKeyedAccount` whose `data` is the SPL Token-2022/Token binary layout,
    /// base64-encoded, exercising the `data.decode()` + `TokenAccount::unpack` path.
    fn base64_account(mint: Pubkey, amount: u64) -> String {
        let account = TokenAccount {
            mint,
            owner: Pubkey::new_unique(),
            amount,
            delegate: COption::None,
            state: AccountState::Initialized,
            is_native: COption::None,
            delegated_amount: 0,
            close_authority: COption::None,
        };
        let mut buf = vec![0u8; TokenAccount::LEN];
        account.pack_into_slice(&mut buf);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&buf);
        format!(
            r#"{{"pubkey":"{ata}","account":{{"lamports":2039280,"owner":"{prog}","executable":false,"rentEpoch":0,"space":165,"data":["{b64}","base64"]}}}}"#,
            ata = Pubkey::new_unique(),
            prog = spl_token::id(),
        )
    }

    /// One `RpcKeyedAccount` whose `data` is jsonParsed, exercising the JSON path.
    fn json_parsed_account(mint: Pubkey, amount: u64) -> String {
        format!(
            r#"{{"pubkey":"{ata}","account":{{"lamports":2039280,"owner":"{prog}","executable":false,"rentEpoch":0,"space":165,"data":{{"program":"spl-token","space":165,"parsed":{{"type":"account","info":{{"mint":"{mint}","owner":"{owner}","tokenAmount":{{"amount":"{amount}","decimals":6,"uiAmount":null,"uiAmountString":"{amount}"}}}}}}}}}}}}"#,
            ata = Pubkey::new_unique(),
            prog = spl_token::id(),
            owner = Pubkey::new_unique(),
        )
    }

    /// One `RpcKeyedAccount` whose `data` carries the legacy `binary` encoding tag,
    /// which `decode()` cannot handle and which is not jsonParsed: the unrecognised
    /// branch the sweep skips with a warning instead of erroring.
    fn unrecognised_encoding_account() -> String {
        format!(
            r#"{{"pubkey":"{ata}","account":{{"lamports":1,"owner":"{prog}","executable":false,"rentEpoch":0,"space":4,"data":["AAAA","binary"]}}}}"#,
            ata = Pubkey::new_unique(),
            prog = spl_token::id(),
        )
    }

    fn result_body(values: &[String]) -> String {
        format!(
            r#"{{"jsonrpc":"2.0","result":{{"context":{{"slot":1}},"value":[{}]}},"id":1}}"#,
            values.join(",")
        )
    }

    const EMPTY_BODY: &str =
        r#"{"jsonrpc":"2.0","result":{"context":{"slot":1},"value":[]},"id":1}"#;

    /// The sweep calls `get_token_accounts_by_owner` once per token program. Route the
    /// SPL Token call (matched by its program id in the request body) to `spl_accounts`
    /// and the Token-2022 call to an empty list so the two are not double-counted.
    async fn mock_sweep(server: &mut mockito::Server, spl_accounts: &[String]) {
        server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex(spl_token::id().to_string()))
            .with_status(200)
            .with_body(result_body(spl_accounts))
            .create_async()
            .await;
        server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex(spl_token_2022::id().to_string()))
            .with_status(200)
            .with_body(EMPTY_BODY)
            .create_async()
            .await;
    }

    #[tokio::test]
    async fn json_parsed_sums_multiple_accounts_per_mint() {
        let mut server = mockito::Server::new_async().await;
        let mint1 = Pubkey::new_unique();
        let mint2 = Pubkey::new_unique();
        // mint1 split across two accounts (100 + 200), mint2 in one (500).
        mock_sweep(
            &mut server,
            &[
                json_parsed_account(mint1, 100),
                json_parsed_account(mint1, 200),
                json_parsed_account(mint2, 500),
            ],
        )
        .await;

        let balances = fetch_escrow_balances_by_mint(&client(&server.url()), Pubkey::new_unique())
            .await
            .unwrap();

        assert_eq!(balances.len(), 2);
        assert_eq!(balances[&mint1], 300, "same mint across accounts must sum");
        assert_eq!(balances[&mint2], 500);
    }

    #[tokio::test]
    async fn decodes_base64_binary_accounts() {
        let mut server = mockito::Server::new_async().await;
        let mint = Pubkey::new_unique();
        mock_sweep(&mut server, &[base64_account(mint, 1_234)]).await;

        let balances = fetch_escrow_balances_by_mint(&client(&server.url()), Pubkey::new_unique())
            .await
            .unwrap();

        assert_eq!(balances[&mint], 1_234, "base64 layout must unpack and sum");
    }

    #[tokio::test]
    async fn skips_unrecognised_encoding_without_erroring() {
        let mut server = mockito::Server::new_async().await;
        let mint = Pubkey::new_unique();
        // A valid account plus one with an unrecognised encoding: the latter is skipped,
        // not fatal, so the valid balance still lands.
        mock_sweep(
            &mut server,
            &[
                json_parsed_account(mint, 50),
                unrecognised_encoding_account(),
            ],
        )
        .await;

        let balances = fetch_escrow_balances_by_mint(&client(&server.url()), Pubkey::new_unique())
            .await
            .unwrap();

        assert_eq!(balances.len(), 1);
        assert_eq!(balances[&mint], 50);
    }

    #[tokio::test]
    async fn empty_owner_returns_empty_map() {
        let mut server = mockito::Server::new_async().await;
        mock_sweep(&mut server, &[]).await;

        let balances = fetch_escrow_balances_by_mint(&client(&server.url()), Pubkey::new_unique())
            .await
            .unwrap();

        assert!(balances.is_empty());
    }

    /// getAccountInfo response wrapping an 82-byte SPL Mint blob.
    fn mint_account_body(
        supply: u64,
        mint_authority: COption<Pubkey>,
        freeze_authority: COption<Pubkey>,
        is_initialized: bool,
    ) -> String {
        let mint = Mint {
            mint_authority,
            supply,
            decimals: 6,
            is_initialized,
            freeze_authority,
        };
        let mut buf = vec![0u8; Mint::LEN];
        mint.pack_into_slice(&mut buf);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&buf);
        format!(
            r#"{{"jsonrpc":"2.0","id":1,"result":{{"context":{{"slot":1}},"value":{{"owner":"{prog}","lamports":1000000,"data":["{b64}","base64"],"executable":false,"rentEpoch":0}}}}}}"#,
            prog = spl_token::id(),
        )
    }

    /// An initialized mint with both authorities set to the same pubkey, which
    /// is how `InitializeMintBuilder` creates every channel mint.
    fn initialized_mint_body(supply: u64, authority: Pubkey) -> String {
        mint_account_body(
            supply,
            COption::Some(authority),
            COption::Some(authority),
            true,
        )
    }

    async fn mock_account_response(server: &mut mockito::ServerGuard, body: String) {
        server
            .mock("POST", "/")
            .with_status(200)
            .with_body(body)
            .create_async()
            .await;
    }

    #[tokio::test]
    async fn fetch_channel_mint_state_decodes_supply_and_authorities() {
        let mut server = mockito::Server::new_async().await;
        let authority = Pubkey::new_unique();
        mock_account_response(&mut server, initialized_mint_body(1_234_567, authority)).await;

        let state = fetch_channel_mint_state(
            &client(&server.url()),
            &Pubkey::new_unique(),
            CommitmentConfig::finalized(),
        )
        .await
        .unwrap();

        assert_eq!(state.supply, 1_234_567);
        assert_eq!(state.mint_authority, Some(authority));
        assert_eq!(state.freeze_authority, Some(authority));
        assert!(state.initialized);
    }

    /// A live mint whose authority was cleared via SetAuthority. `initialized`
    /// stays true so callers can separate this from a mint that was never
    /// created.
    #[tokio::test]
    async fn fetch_channel_mint_state_cleared_authority_is_initialized() {
        let mut server = mockito::Server::new_async().await;
        let body = mint_account_body(500, COption::None, COption::None, true);
        mock_account_response(&mut server, body).await;

        let state = fetch_channel_mint_state(
            &client(&server.url()),
            &Pubkey::new_unique(),
            CommitmentConfig::finalized(),
        )
        .await
        .unwrap();

        assert!(state.initialized);
        assert_eq!(state.mint_authority, None);
        assert_eq!(state.freeze_authority, None);
        assert_eq!(state.supply, 500);
    }

    /// SPL-Token-owned bytes that decode but carry `is_initialized = false`
    /// are not corruption, they are a mint that does not exist yet.
    #[tokio::test]
    async fn fetch_channel_mint_state_uninitialized_bytes_are_not_an_error() {
        let mut server = mockito::Server::new_async().await;
        let body = mint_account_body(0, COption::None, COption::None, false);
        mock_account_response(&mut server, body).await;

        let state = fetch_channel_mint_state(
            &client(&server.url()),
            &Pubkey::new_unique(),
            CommitmentConfig::finalized(),
        )
        .await
        .unwrap();

        assert!(!state.initialized);
    }

    #[tokio::test]
    async fn fetch_channel_mint_state_absent_mint_is_zero() {
        let mut server = mockito::Server::new_async().await;
        // A null account value: the channel mint does not exist yet -> 0 supply.
        let body =
            r#"{"jsonrpc":"2.0","id":1,"result":{"context":{"slot":1},"value":null}}"#.to_string();
        mock_account_response(&mut server, body).await;

        let state = fetch_channel_mint_state(
            &client(&server.url()),
            &Pubkey::new_unique(),
            CommitmentConfig::finalized(),
        )
        .await
        .unwrap();

        assert_eq!(
            state.supply, 0,
            "absent mint account must read as zero supply"
        );
        assert!(!state.initialized);
    }

    #[tokio::test]
    async fn fetch_channel_mint_state_rpc_error_is_err() {
        // A transport/node error must surface as Err, never Ok(0): the plain
        // get_account convenience formats a 503 with the same AccountNotFound
        // prefix as a genuinely-absent mint, which would let an RPC outage
        // masquerade as zero supply and blind the invariant.
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/")
            .with_status(503)
            .create_async()
            .await;

        let fast = RpcClientWithRetry::with_retry_config(
            server.url(),
            RetryConfig {
                max_attempts: 1,
                base_delay: std::time::Duration::from_millis(1),
                max_delay: std::time::Duration::from_millis(1),
            },
            CommitmentConfig::finalized(),
        );
        let result =
            fetch_channel_mint_state(&fast, &Pubkey::new_unique(), CommitmentConfig::finalized())
                .await;
        assert!(result.is_err(), "an RPC outage must be Err, not Ok(0)");
    }

    #[tokio::test]
    async fn errors_on_malformed_json_account() {
        let mut server = mockito::Server::new_async().await;
        // jsonParsed account missing the `tokenAmount` field: a corrupt response must
        // surface as an error, never a silently dropped balance.
        let malformed = format!(
            r#"{{"pubkey":"{ata}","account":{{"lamports":1,"owner":"{prog}","executable":false,"rentEpoch":0,"space":165,"data":{{"program":"spl-token","space":165,"parsed":{{"type":"account","info":{{"mint":"{mint}","owner":"{prog}"}}}}}}}}}}"#,
            ata = Pubkey::new_unique(),
            prog = spl_token::id(),
            mint = Pubkey::new_unique(),
        );
        mock_sweep(&mut server, &[malformed]).await;

        let result =
            fetch_escrow_balances_by_mint(&client(&server.url()), Pubkey::new_unique()).await;

        let err = result.expect_err("malformed account must error").reason;
        assert!(err.contains("tokenAmount"), "unexpected error: {err}");
    }
}
