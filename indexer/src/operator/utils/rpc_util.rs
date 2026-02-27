use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_client::GetConfirmedSignaturesForAddress2Config;
use solana_rpc_client_api::client_error;
use solana_rpc_client_api::config::RpcTransactionConfig;
use solana_sdk::account::Account;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::hash::Hash;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signature;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use tracing::warn;

use crate::operator::utils::instruction_util::RetryPolicy;

const DEFAULT_MAX_ATTEMPTS: u32 = 5;
const DEFAULT_BASE_DELAY: Duration = Duration::from_millis(100);
const DEFAULT_MAX_DELAY: Duration = Duration::from_secs(10);

/// Configuration for RPC retry behavior
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of retry attempts
    pub max_attempts: u32,
    /// Base delay between retries (exponential backoff applied)
    pub base_delay: Duration,
    /// Maximum delay between retries
    pub max_delay: Duration,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            base_delay: DEFAULT_BASE_DELAY,
            max_delay: DEFAULT_MAX_DELAY,
        }
    }
}

pub struct RpcClientWithRetry {
    pub rpc_client: Arc<RpcClient>,
    pub retry_config: RetryConfig,
}

impl RpcClientWithRetry {
    /// Create a new RPC client with custom retry config
    pub fn with_retry_config(
        url: String,
        retry_config: RetryConfig,
        commitment: CommitmentConfig,
    ) -> Self {
        Self {
            rpc_client: Arc::new(RpcClient::new_with_commitment(url, commitment)),
            retry_config,
        }
    }

    /// Execute an RPC operation with configurable retry behavior
    ///
    /// # Arguments
    /// * `operation_name` - Name for logging/debugging
    /// * `retry_policy` - Controls retry behavior (None or Idempotent)
    /// * `f` - Async operation to execute/retry
    ///
    /// # Returns
    /// Result from the operation or MaxRetriesExceeded error
    pub async fn with_retry<F, Fut, T, E>(
        &self,
        operation_name: &str,
        retry_policy: RetryPolicy,
        f: F,
    ) -> Result<T, Box<client_error::Error>>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T, E>>,
        E: std::fmt::Display + Into<Box<client_error::Error>>,
    {
        match retry_policy {
            RetryPolicy::None => {
                // Single attempt - no retry
                f().await.map_err(|e| e.into())
            }
            RetryPolicy::Idempotent => {
                // Retry with exponential backoff
                let mut attempts = 0;

                loop {
                    attempts += 1;

                    match f().await {
                        Ok(result) => {
                            return Ok(result);
                        }
                        Err(e) => {
                            let last_error = e.to_string();

                            if attempts >= self.retry_config.max_attempts {
                                warn!(
                                    "{} failed after {} attempts: {}",
                                    operation_name, attempts, last_error
                                );
                                return Err(e.into());
                            }

                            let delay = self.retry_config.base_delay * 2_u32.pow(attempts - 1);
                            let delay = delay.min(self.retry_config.max_delay);

                            sleep(delay).await;
                        }
                    }
                }
            }
        }
    }

    /// Get recent blockhash with retry
    pub async fn get_latest_blockhash(&self) -> Result<Hash, Box<client_error::Error>> {
        self.with_retry("get_latest_blockhash", RetryPolicy::Idempotent, || async {
            self.rpc_client.get_latest_blockhash().await
        })
        .await
    }

    /// Send transaction with configurable retry policy
    ///
    /// # Arguments
    /// * `transaction` - The transaction to send
    /// * `retry_policy` - Controls retry behavior:
    ///   - `RetryPolicy::None`: Single attempt, no retry (for non-idempotent operations)
    ///   - `RetryPolicy::Idempotent`: Retry with exponential backoff (for idempotent operations)
    ///
    /// # Safety
    /// For operations that can duplicate side effects (for example mint sends), use
    /// `RetryPolicy::None` at send time and add an external idempotency check before resubmission.
    /// Only use retry for operations that are safe to execute multiple times.
    pub async fn send_transaction(
        &self,
        transaction: &solana_sdk::transaction::Transaction,
        retry_policy: RetryPolicy,
    ) -> Result<solana_sdk::signature::Signature, Box<client_error::Error>> {
        self.with_retry("send_transaction", retry_policy, || async {
            self.rpc_client.send_transaction(transaction).await
        })
        .await
    }

    /// Get account with retry
    pub async fn get_account_data(
        &self,
        pubkey: &Pubkey,
    ) -> Result<Vec<u8>, Box<client_error::Error>> {
        self.with_retry("get_account_info", RetryPolicy::Idempotent, || async {
            self.rpc_client.get_account_data(pubkey).await
        })
        .await
    }

    /// Get account with retry
    pub async fn get_account(&self, pubkey: &Pubkey) -> Result<Account, Box<client_error::Error>> {
        self.with_retry("get_account", RetryPolicy::Idempotent, || async {
            self.rpc_client.get_account(pubkey).await
        })
        .await
    }

    /// Get signature statuses with retry (read-only, always safe to retry)
    pub async fn get_signature_statuses(
        &self,
        signatures: &[Signature],
    ) -> Result<
        solana_client::rpc_response::Response<
            Vec<Option<solana_transaction_status::TransactionStatus>>,
        >,
        Box<client_error::Error>,
    > {
        self.with_retry(
            "get_signature_statuses",
            RetryPolicy::Idempotent,
            || async { self.rpc_client.get_signature_statuses(signatures).await },
        )
        .await
    }

    /// Get recent signatures that touched an address (read-only, safe to retry)
    pub async fn get_signatures_for_address(
        &self,
        address: &Pubkey,
        limit: usize,
    ) -> Result<
        Vec<solana_rpc_client_api::response::RpcConfirmedTransactionStatusWithSignature>,
        Box<client_error::Error>,
    > {
        self.with_retry(
            "get_signatures_for_address",
            RetryPolicy::Idempotent,
            || async {
                let config = GetConfirmedSignaturesForAddress2Config {
                    before: None,
                    until: None,
                    limit: Some(limit),
                    commitment: Some(CommitmentConfig::confirmed()),
                };

                self.rpc_client
                    .get_signatures_for_address_with_config(address, config)
                    .await
            },
        )
        .await
    }

    /// Get a confirmed transaction in JSON-parsed encoding (read-only, safe to retry)
    pub async fn get_transaction(
        &self,
        signature: &Signature,
    ) -> Result<
        solana_transaction_status::EncodedConfirmedTransactionWithStatusMeta,
        Box<client_error::Error>,
    > {
        let config = RpcTransactionConfig {
            encoding: Some(solana_transaction_status::UiTransactionEncoding::JsonParsed),
            commitment: Some(CommitmentConfig::confirmed()),
            max_supported_transaction_version: Some(0),
        };

        self.with_retry("get_transaction", RetryPolicy::Idempotent, || async {
            self.rpc_client
                .get_transaction_with_config(signature, config)
                .await
        })
        .await
    }
}
