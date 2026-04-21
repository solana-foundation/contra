use crate::error::{AccountError, OperatorError};
use crate::operator::RpcClientWithRetry;
use crate::storage::Storage;
use solana_sdk::pubkey::Pubkey;
use spl_token::ID as TOKEN_PROGRAM_ID;
use spl_token_2022::extension::{
    pausable::PausableConfig, BaseStateWithExtensions, StateWithExtensions,
};
use spl_token_2022::state::Mint as Token2022MintState;
use spl_token_2022::ID as TOKEN_2022_PROGRAM_ID;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

const DECIMALS_OFFSET: usize = 44;

/// In-memory cache for mint metadata (`token_program`, `decimals`,
/// `is_pausable`). On a miss, falls through to the DB; if the DB row
/// exists but `is_pausable` hasn't been resolved yet (indexer writes it
/// as `None` at `AllowMint` time), the cache RPC-fetches the on-chain
/// mint to detect the Token-2022 `PausableConfig` extension, writes the
/// resolved value back via `set_mint_pausable`, and caches it. Also
/// exposes `check_paused` for the dynamic `PausableConfig.paused` check
/// that the release-funds pre-flight needs.
pub struct MintCache {
    storage: Arc<Storage>,
    rpc_client: Option<Arc<RpcClientWithRetry>>,
    cache: HashMap<String, MintMetadata>,
}

/// Cached mint metadata
#[derive(Clone, Debug, PartialEq)]
pub struct MintMetadata {
    pub token_program: Pubkey,
    pub decimals: u8,
    /// True iff the on-chain mint carries the Token-2022 PausableConfig
    /// extension. This is a static property of the mint; the *current*
    /// `PausableConfig.paused` bool is dynamic and must be re-fetched —
    /// see `MintCache::check_paused`.
    pub is_pausable: bool,
}

impl MintCache {
    pub fn new(storage: Arc<Storage>) -> Self {
        Self {
            storage,
            rpc_client: None,
            cache: HashMap::new(),
        }
    }

    pub fn with_rpc(storage: Arc<Storage>, rpc_client: Arc<RpcClientWithRetry>) -> Self {
        Self {
            storage,
            rpc_client: Some(rpc_client),
            cache: HashMap::new(),
        }
    }

    /// Get mint metadata, resolving and caching `is_pausable` on first sight.
    ///
    /// Order:
    /// 1. In-memory cache hit → return.
    /// 2. Storage row with `is_pausable = Some(_)` → return (fast path).
    /// 3. Anything else (no row, or `is_pausable = None`) → RPC-fetch the
    ///    mint account, parse the PausableConfig extension presence, and
    ///    write the result back to storage so subsequent restarts skip
    ///    the RPC. Write-back is skipped if no row exists yet — the
    ///    indexer is expected to land the row shortly.
    pub async fn get_mint_metadata(
        &mut self,
        mint: &Pubkey,
    ) -> Result<MintMetadata, OperatorError> {
        let mint_str = mint.to_string();

        if let Some(metadata) = self.cache.get(&mint_str) {
            return Ok(metadata.clone());
        }

        let db_mint = self.storage.get_mint(&mint_str).await?;

        if let Some(ref m) = db_mint {
            if let Some(is_pausable) = m.is_pausable {
                let token_program = Pubkey::from_str(&m.token_program).map_err(|e| {
                    OperatorError::InvalidPubkey {
                        pubkey: m.token_program.clone(),
                        reason: e.to_string(),
                    }
                })?;
                let metadata = MintMetadata {
                    token_program,
                    decimals: m.decimals as u8,
                    is_pausable,
                };
                self.cache.insert(mint_str, metadata.clone());
                return Ok(metadata);
            }
        }

        let rpc = self.rpc_client.as_ref().ok_or_else(|| {
            OperatorError::RpcError(format!(
                "MintCache needs RPC to resolve mint {mint_str} (storage row {}), but no RPC client is configured",
                if db_mint.is_some() { "lacks is_pausable" } else { "is absent" },
            ))
        })?;

        let metadata = self.fetch_mint_from_rpc(mint, rpc).await?;

        // Only persist is_pausable if the indexer has already landed a row.
        // The row is the authoritative source for is_pausable going forward;
        // no row means this is a pre-AllowMint-ingested edge case, so we
        // keep the resolution in-memory only.
        if db_mint.is_some() {
            self.storage
                .set_mint_pausable(&mint_str, metadata.is_pausable)
                .await?;
        }

        self.cache.insert(mint_str, metadata.clone());
        Ok(metadata)
    }

    /// Live check of the `PausableConfig.paused` flag. Intended for the
    /// pre-flight pause check in the operator's ReleaseFunds path: only
    /// call this after `MintMetadata.is_pausable` came back true.
    pub async fn check_paused(&self, mint: &Pubkey) -> Result<bool, OperatorError> {
        let rpc = self.rpc_client.as_ref().ok_or_else(|| {
            OperatorError::RpcError("check_paused requires an RPC client".to_string())
        })?;

        let account = rpc
            .get_account(mint)
            .await
            .map_err(|_| AccountError::AccountNotFound { pubkey: *mint })?;

        let state = StateWithExtensions::<Token2022MintState>::unpack(&account.data).map_err(
            |_| AccountError::InvalidMint {
                pubkey: *mint,
                reason: "failed to parse Token-2022 mint".to_string(),
            },
        )?;

        let cfg = state.get_extension::<PausableConfig>().map_err(|_| {
            AccountError::InvalidMint {
                pubkey: *mint,
                reason: "mint is tagged is_pausable but PausableConfig extension is missing"
                    .to_string(),
            }
        })?;

        Ok(bool::from(cfg.paused))
    }

    async fn fetch_mint_from_rpc(
        &self,
        mint: &Pubkey,
        rpc: &RpcClientWithRetry,
    ) -> Result<MintMetadata, OperatorError> {
        let account = rpc
            .get_account(mint)
            .await
            .map_err(|_| AccountError::AccountNotFound { pubkey: *mint })?;

        let token_program = account.owner;

        if ![TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID].contains(&token_program) {
            return Err(AccountError::InvalidMint {
                pubkey: *mint,
                reason: format!("Invalid mint owner: {}", account.owner),
            }
            .into());
        }

        // Parse SPL token mint data directly (decimals is at offset 44 for both SPL and T22)
        // Mint layout: [option(coption_authority): 36 bytes, supply: 8 bytes, decimals: 1 byte, ...]
        if account.data.len() < DECIMALS_OFFSET + 1 {
            return Err(AccountError::InvalidMint {
                pubkey: *mint,
                reason: format!("Invalid mint account data length: {}", account.data.len()),
            }
            .into());
        }

        let decimals = account.data[DECIMALS_OFFSET];

        // PausableConfig can only exist on Token-2022 mints. If the account
        // isn't well-formed for the extension parser (e.g. an uninitialized
        // base Mint in a test fixture), we conservatively treat it as
        // "no extension" rather than erroring — callers only need
        // is_pausable to gate the pre-flight pause check.
        let is_pausable = if token_program == TOKEN_2022_PROGRAM_ID {
            StateWithExtensions::<Token2022MintState>::unpack(&account.data)
                .map(|m| m.get_extension::<PausableConfig>().is_ok())
                .unwrap_or(false)
        } else {
            false
        };

        Ok(MintMetadata {
            token_program,
            decimals,
            is_pausable,
        })
    }

    /// Pre-populate cache with mint metadata
    pub async fn prefetch_mints(&mut self, mints: &[Pubkey]) -> Result<(), OperatorError> {
        for mint in mints {
            self.get_mint_metadata(mint).await?;
        }
        Ok(())
    }

    // For now contra only supports SPL, when we want to make the move to token 2022, we
    // can call get mint_metadata above instead of this function.
    pub fn get_contra_token_program(&self) -> Pubkey {
        TOKEN_PROGRAM_ID
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operator::rpc_util::RpcClientWithRetry;
    use crate::operator::RetryConfig;
    use crate::storage::common::models::DbMint;
    use crate::storage::common::storage::mock::MockStorage;
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use solana_client::nonblocking::rpc_client::RpcClient;
    use solana_client::rpc_request::RpcRequest;
    use solana_sdk::pubkey::Pubkey;
    use spl_token_2022::ID as TOKEN_2022_PROGRAM_ID;

    impl MintCache {
        pub fn clear(&mut self) {
            self.cache.clear();
        }

        pub fn cache_size(&self) -> usize {
            self.cache.len()
        }
    }

    impl RpcClientWithRetry {
        pub fn new_mocked(mocks: solana_client::rpc_client::Mocks) -> Self {
            Self {
                rpc_client: Arc::new(RpcClient::new_mock_with_mocks(
                    "http://127.0.0.1:8899".to_string(),
                    mocks,
                )),
                retry_config: RetryConfig::default(),
            }
        }
    }

    fn create_mock_mint_account_data(decimals: u8) -> Vec<u8> {
        let mut data = vec![0u8; 82];
        data[DECIMALS_OFFSET] = decimals;
        data
    }

    fn create_test_mint() -> Pubkey {
        Pubkey::new_unique()
    }

    // Helper to create a mocked RPC response for getAccountInfo
    fn create_mock_account_response(mint_owner: &Pubkey, decimals: u8) -> serde_json::Value {
        let mint_data = create_mock_mint_account_data(decimals);

        serde_json::json!({
            "context": {"slot": 1},
            "value": {
                "owner": mint_owner.to_string(),
                "lamports": 1000000,
                "data": [STANDARD.encode(&mint_data), "base64"],
                "executable": false,
                "rentEpoch": 0
            }
        })
    }

    fn create_test_storage_with_mint(
        mint: &Pubkey,
        token_program: &Pubkey,
        decimals: i16,
    ) -> Arc<Storage> {
        let mut mock = MockStorage::new();

        mock.add_mint(DbMint {
            mint_address: mint.to_string(),
            decimals,
            token_program: token_program.to_string(),
            created_at: chrono::Utc::now(),
            is_pausable: Some(false),
        });

        Arc::new(Storage::Mock(mock))
    }

    #[tokio::test]
    async fn test_cache_miss_then_hit() {
        let mint = create_test_mint();
        let token_program = TOKEN_PROGRAM_ID;
        let storage = create_test_storage_with_mint(&mint, &token_program, 6);

        let mut cache = MintCache::new(storage);

        assert_eq!(cache.cache_size(), 0);

        // First call - cache miss, fetches from storage
        let metadata1 = cache.get_mint_metadata(&mint).await.unwrap();
        assert_eq!(metadata1.token_program, token_program);
        assert_eq!(metadata1.decimals, 6);
        assert_eq!(cache.cache_size(), 1);

        // Second call - cache hit, no storage fetch
        let metadata2 = cache.get_mint_metadata(&mint).await.unwrap();
        assert_eq!(metadata2, metadata1);
        assert_eq!(cache.cache_size(), 1);
    }

    #[tokio::test]
    async fn test_token_2022_mint() {
        let mint = create_test_mint();
        let token_program = TOKEN_2022_PROGRAM_ID;
        let storage = create_test_storage_with_mint(&mint, &token_program, 9);

        let mut cache = MintCache::new(storage);

        let metadata = cache.get_mint_metadata(&mint).await.unwrap();
        assert_eq!(metadata.token_program, TOKEN_2022_PROGRAM_ID);
        assert_eq!(metadata.decimals, 9);
    }

    #[tokio::test]
    async fn test_mint_not_found() {
        let mint = create_test_mint();
        let storage = Arc::new(Storage::Mock(MockStorage::new()));

        let mut cache = MintCache::new(storage);

        let result = cache.get_mint_metadata(&mint).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_prefetch_mints() {
        let mint1 = create_test_mint();
        let mint2 = create_test_mint();
        let mint3 = create_test_mint();

        let mut mock = MockStorage::new();
        for mint in [&mint1, &mint2, &mint3] {
            mock.add_mint(DbMint {
                mint_address: mint.to_string(),
                decimals: 6,
                token_program: TOKEN_PROGRAM_ID.to_string(),
                created_at: chrono::Utc::now(),
                is_pausable: Some(false),
            });
        }

        let storage = Arc::new(Storage::Mock(mock));
        let mut cache = MintCache::new(storage);

        assert_eq!(cache.cache_size(), 0);

        cache.prefetch_mints(&[mint1, mint2, mint3]).await.unwrap();
        assert_eq!(cache.cache_size(), 3);

        let _ = cache.get_mint_metadata(&mint1).await.unwrap();
        let _ = cache.get_mint_metadata(&mint2).await.unwrap();
        let _ = cache.get_mint_metadata(&mint3).await.unwrap();
        assert_eq!(cache.cache_size(), 3);
    }

    #[tokio::test]
    async fn test_multiple_mints_different_programs() {
        let spl_mint = create_test_mint();
        let t22_mint = create_test_mint();

        let mut mock = MockStorage::new();
        mock.add_mint(DbMint {
            mint_address: spl_mint.to_string(),
            decimals: 6,
            token_program: TOKEN_PROGRAM_ID.to_string(),
            created_at: chrono::Utc::now(),
            is_pausable: Some(false),
        });
        mock.add_mint(DbMint {
            mint_address: t22_mint.to_string(),
            decimals: 9,
            token_program: TOKEN_2022_PROGRAM_ID.to_string(),
            created_at: chrono::Utc::now(),
            is_pausable: Some(false),
        });

        let storage = Arc::new(Storage::Mock(mock));
        let mut cache = MintCache::new(storage);

        let spl_metadata = cache.get_mint_metadata(&spl_mint).await.unwrap();
        assert_eq!(spl_metadata.token_program, TOKEN_PROGRAM_ID);
        assert_eq!(spl_metadata.decimals, 6);

        let t22_metadata = cache.get_mint_metadata(&t22_mint).await.unwrap();
        assert_eq!(t22_metadata.token_program, TOKEN_2022_PROGRAM_ID);
        assert_eq!(t22_metadata.decimals, 9);

        assert_eq!(cache.cache_size(), 2);
    }

    #[tokio::test]
    async fn test_rpc_fallback_spl_token() {
        let mint = create_test_mint();
        let account_response = create_mock_account_response(&TOKEN_PROGRAM_ID, 9);

        let mut mocks = std::collections::HashMap::new();
        mocks.insert(RpcRequest::GetAccountInfo, account_response);

        let rpc_client = RpcClientWithRetry::new_mocked(mocks);

        let storage = Arc::new(Storage::Mock(MockStorage::new()));
        let mut cache = MintCache::with_rpc(storage, Arc::new(rpc_client));

        // Should fallback to RPC since mint not in storage
        let metadata = cache.get_mint_metadata(&mint).await.unwrap();
        assert_eq!(metadata.token_program, TOKEN_PROGRAM_ID);
        assert_eq!(metadata.decimals, 9);
        assert_eq!(cache.cache_size(), 1);
    }

    #[tokio::test]
    async fn test_rpc_fallback_token_2022() {
        let mint = create_test_mint();
        let account_response = create_mock_account_response(&TOKEN_2022_PROGRAM_ID, 6);

        let mut mocks = std::collections::HashMap::new();
        mocks.insert(RpcRequest::GetAccountInfo, account_response);

        let rpc_client = RpcClientWithRetry::new_mocked(mocks);

        let storage = Arc::new(Storage::Mock(MockStorage::new()));
        let mut cache = MintCache::with_rpc(storage, Arc::new(rpc_client));

        // Should fallback to RPC and detect Token-2022
        let metadata = cache.get_mint_metadata(&mint).await.unwrap();
        assert_eq!(metadata.token_program, TOKEN_2022_PROGRAM_ID);
        assert_eq!(metadata.decimals, 6);
    }

    #[tokio::test]
    async fn storage_row_without_is_pausable_triggers_rpc_resolution_and_write_back() {
        let mint = create_test_mint();

        // Indexer has landed the mints row but the operator hasn't resolved
        // is_pausable yet — this is the state we need to lazily fill.
        let mock_storage = MockStorage::new();
        mock_storage.mints.lock().unwrap().insert(
            mint.to_string(),
            DbMint {
                mint_address: mint.to_string(),
                decimals: 6,
                token_program: TOKEN_PROGRAM_ID.to_string(),
                created_at: chrono::Utc::now(),
                is_pausable: None,
            },
        );

        // Plain SPL Token mint on RPC → no extensions → is_pausable=false.
        let account_response = create_mock_account_response(&TOKEN_PROGRAM_ID, 6);
        let mut mocks = std::collections::HashMap::new();
        mocks.insert(RpcRequest::GetAccountInfo, account_response);
        let rpc_client = RpcClientWithRetry::new_mocked(mocks);

        let storage = Arc::new(Storage::Mock(mock_storage.clone()));
        let mut cache = MintCache::with_rpc(storage, Arc::new(rpc_client));

        let metadata = cache.get_mint_metadata(&mint).await.unwrap();
        assert!(!metadata.is_pausable);

        // Write-back happened — subsequent reads don't need RPC.
        let stored = mock_storage
            .mints
            .lock()
            .unwrap()
            .get(&mint.to_string())
            .cloned()
            .expect("mint row should still exist after write-back");
        assert_eq!(stored.is_pausable, Some(false));
    }

    #[tokio::test]
    async fn get_mint_metadata_errors_when_is_pausable_unresolved_and_no_rpc() {
        let mint = create_test_mint();

        // DB row lacks is_pausable — resolution would need RPC, which is absent.
        let mock_storage = MockStorage::new();
        mock_storage.mints.lock().unwrap().insert(
            mint.to_string(),
            DbMint {
                mint_address: mint.to_string(),
                decimals: 6,
                token_program: TOKEN_PROGRAM_ID.to_string(),
                created_at: chrono::Utc::now(),
                is_pausable: None,
            },
        );

        let storage = Arc::new(Storage::Mock(mock_storage));
        let mut cache = MintCache::new(storage);

        let err = cache
            .get_mint_metadata(&mint)
            .await
            .expect_err("should error without RPC");
        assert!(
            matches!(err, crate::error::OperatorError::RpcError(_)),
            "expected RpcError, got {err:?}",
        );
    }

    #[tokio::test]
    async fn check_paused_errors_without_rpc() {
        let storage = Arc::new(Storage::Mock(MockStorage::new()));
        let cache = MintCache::new(storage);

        let err = cache
            .check_paused(&create_test_mint())
            .await
            .expect_err("check_paused should require RPC");
        assert!(
            matches!(err, crate::error::OperatorError::RpcError(_)),
            "expected RpcError, got {err:?}",
        );
    }

    #[tokio::test]
    async fn test_rpc_fallback_invalid_owner() {
        let mint = create_test_mint();
        let invalid_owner = Pubkey::new_unique();
        let account_response = create_mock_account_response(&invalid_owner, 6);

        let mut mocks = std::collections::HashMap::new();
        mocks.insert(RpcRequest::GetAccountInfo, account_response);

        let rpc_client = RpcClientWithRetry::new_mocked(mocks);

        let storage = Arc::new(Storage::Mock(MockStorage::new()));
        let mut cache = MintCache::with_rpc(storage, Arc::new(rpc_client));

        // Should error on invalid owner
        let result = cache.get_mint_metadata(&mint).await;
        assert!(result.is_err());
    }
}
