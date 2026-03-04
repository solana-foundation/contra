use crate::error::{AccountError, OperatorError, StorageError};
use crate::operator::RpcClientWithRetry;
use crate::storage::Storage;
use solana_sdk::pubkey::Pubkey;
use spl_token::ID as TOKEN_PROGRAM_ID;
use spl_token_2022::ID as TOKEN_2022_PROGRAM_ID;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

const DECIMALS_OFFSET: usize = 44;

/// In-memory cache for mint metadata (token_program and decimals)
/// Fetches from storage once and caches for subsequent lookups
/// Falls back to on-chain RPC if not in storage
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

    /// Get mint metadata from cache or fetch from storage
    /// Falls back to RPC if not in storage
    pub async fn get_mint_metadata(
        &mut self,
        mint: &Pubkey,
    ) -> Result<MintMetadata, OperatorError> {
        let mint_str = mint.to_string();

        // Check cache first
        if let Some(metadata) = self.cache.get(&mint_str) {
            return Ok(metadata.clone());
        }

        // Try storage
        if let Some(db_mint) = self.storage.get_mint(&mint_str).await? {
            let token_program = Pubkey::from_str(&db_mint.token_program).map_err(|e| {
                OperatorError::InvalidPubkey {
                    pubkey: db_mint.token_program.clone(),
                    reason: e.to_string(),
                }
            })?;

            let metadata = MintMetadata {
                token_program,
                decimals: db_mint.decimals as u8,
            };

            self.cache.insert(mint_str, metadata.clone());
            return Ok(metadata);
        }

        // Fallback to RPC if available
        if let Some(rpc) = &self.rpc_client {
            let metadata = self.fetch_mint_from_rpc(mint, rpc).await?;
            self.cache.insert(mint_str, metadata.clone());
            return Ok(metadata);
        }

        Err(StorageError::DatabaseError {
            message: format!("Mint not found in storage: {}", mint_str),
        }
        .into())
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

        // Determine token program from account owner
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

        Ok(MintMetadata {
            token_program,
            decimals,
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
        });
        mock.add_mint(DbMint {
            mint_address: t22_mint.to_string(),
            decimals: 9,
            token_program: TOKEN_2022_PROGRAM_ID.to_string(),
            created_at: chrono::Utc::now(),
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
