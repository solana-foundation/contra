use crate::error::DataSourceRpcError;
use crate::indexer::datasource::rpc_polling::types::RpcBlock;
use futures::future::join_all;
use serde_json::json;
use solana_sdk::commitment_config::CommitmentLevel;
use solana_transaction_status::UiTransactionEncoding;

pub struct RpcPoller {
    client: reqwest::Client,
    rpc_url: String,
    encoding: UiTransactionEncoding,
    commitment: CommitmentLevel,
}

impl RpcPoller {
    pub fn new(
        rpc_url: String,
        encoding: UiTransactionEncoding,
        commitment: CommitmentLevel,
    ) -> Self {
        let client = reqwest::Client::new();
        Self {
            client,
            rpc_url,
            encoding,
            commitment,
        }
    }

    /// Get a single block by slot
    async fn get_block(&self, slot: u64) -> Result<Option<RpcBlock>, DataSourceRpcError> {
        let response = self
            .client
            .post(&self.rpc_url)
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "getBlock",
                "params": [
                    slot,
                    {
                        "encoding": self.encoding.to_string(),
                        "transactionDetails": "full",
                        "maxSupportedTransactionVersion": 0,
                        "rewards": false
                    }
                ]
            }))
            .send()
            .await?;

        let json: serde_json::Value = response.json().await?;

        // Check for RPC error
        if let Some(error) = json.get("error") {
            // Slot was skipped or missing:
            // -32007: Slot skipped or missing due to ledger jump to recent snapshot
            // -32009: Slot skipped or missing in long-term storage
            if error["code"] == -32007 || error["code"] == -32009 {
                return Ok(None);
            }
            return Err(DataSourceRpcError::Protocol {
                reason: format!("RPC error: {}", error),
            });
        }

        // Check if result is null (block not available)
        if json["result"].is_null() {
            return Ok(None);
        }

        // Parse the block
        let block: RpcBlock =
            serde_json::from_value(json["result"].clone()).map_err(DataSourceRpcError::from)?;
        Ok(Some(block))
    }

    /// Get multiple blocks in parallel
    pub async fn get_blocks_batch(
        &self,
        slots: Vec<u64>,
    ) -> Vec<(u64, Result<Option<RpcBlock>, DataSourceRpcError>)> {
        let futures = slots.into_iter().map(|slot| async move {
            let result = self.get_block(slot).await;
            (slot, result)
        });

        join_all(futures).await
    }

    /// Get the latest slot
    pub async fn get_latest_slot(&self) -> Result<u64, DataSourceRpcError> {
        let response = self
            .client
            .post(&self.rpc_url)
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "getSlot",
                "params": [{
                    "commitment": self.commitment.to_string()
                  }]
            }))
            .send()
            .await
            .map_err(DataSourceRpcError::from)?;

        let json: serde_json::Value = response.json().await.map_err(DataSourceRpcError::from)?;

        if let Some(error) = json.get("error") {
            return Err(DataSourceRpcError::Protocol {
                reason: format!("RPC error: {}", error),
            });
        }

        json["result"]
            .as_u64()
            .ok_or_else(|| DataSourceRpcError::Protocol {
                reason: "Invalid slot response".to_string(),
            })
    }

    /// Get slot range to process
    pub async fn get_slots_to_process(
        &self,
        from_slot: u64,
        max_slots: usize,
    ) -> Result<Vec<u64>, DataSourceRpcError> {
        let latest_slot = self.get_latest_slot().await?;

        if from_slot >= latest_slot {
            return Ok(vec![]);
        }

        let to_slot = std::cmp::min(from_slot + max_slots as u64, latest_slot);
        Ok((from_slot..to_slot).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::rpc_mocks::*;
    use mockito::Server;

    // ============================================================================
    // get_block Tests
    // ============================================================================

    #[tokio::test]
    async fn test_get_block_success() {
        let mut server = Server::new_async().await;
        let _m = mock_rpc_success(
            &mut server,
            r#"{
                "blockhash": "TestBlockHash11111111111111111111111111111",
                "parentSlot": 100,
                "transactions": []
            }"#,
        )
        .await;

        let poller = RpcPoller::new(
            server.url(),
            UiTransactionEncoding::Json,
            CommitmentLevel::Finalized,
        );
        let result = poller.get_block(101).await;

        assert!(result.is_ok());
        let block = result.unwrap();
        assert!(block.is_some());
        let block = block.unwrap();
        assert_eq!(
            block.blockhash,
            "TestBlockHash11111111111111111111111111111"
        );
        assert_eq!(block.parent_slot, 100);
    }

    #[tokio::test]
    async fn test_get_block_skipped_slot() {
        let mut server = Server::new_async().await;
        let _m = mock_rpc_error(&mut server, -32009, "Slot was skipped").await;

        let poller = RpcPoller::new(
            server.url(),
            UiTransactionEncoding::Json,
            CommitmentLevel::Finalized,
        );
        let result = poller.get_block(101).await;

        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_get_block_null_result() {
        let mut server = Server::new_async().await;
        let _m = mock_rpc_success(&mut server, "null").await;

        let poller = RpcPoller::new(
            server.url(),
            UiTransactionEncoding::Json,
            CommitmentLevel::Finalized,
        );
        let result = poller.get_block(101).await;

        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_get_block_rpc_error() {
        let mut server = Server::new_async().await;
        let _m = mock_rpc_error(&mut server, -32600, "Invalid request").await;

        let poller = RpcPoller::new(
            server.url(),
            UiTransactionEncoding::Json,
            CommitmentLevel::Finalized,
        );
        let result = poller.get_block(101).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("RPC error"));
    }

    // ============================================================================
    // get_latest_slot Tests
    // ============================================================================

    #[tokio::test]
    async fn test_get_latest_slot_success() {
        let mut server = Server::new_async().await;
        let _m = mock_rpc_success(&mut server, "12345").await;

        let poller = RpcPoller::new(
            server.url(),
            UiTransactionEncoding::Json,
            CommitmentLevel::Finalized,
        );
        let result = poller.get_latest_slot().await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 12345);
    }

    #[tokio::test]
    async fn test_get_latest_slot_rpc_error() {
        let mut server = Server::new_async().await;
        let _m = mock_rpc_error(&mut server, -32600, "Invalid request").await;

        let poller = RpcPoller::new(
            server.url(),
            UiTransactionEncoding::Json,
            CommitmentLevel::Finalized,
        );
        let result = poller.get_latest_slot().await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("RPC error"));
    }

    // ============================================================================
    // get_slots_to_process Tests
    // ============================================================================

    #[tokio::test]
    async fn test_get_slots_to_process_normal() {
        let mut server = Server::new_async().await;
        let _m = mock_rpc_success(&mut server, "150").await;

        let poller = RpcPoller::new(
            server.url(),
            UiTransactionEncoding::Json,
            CommitmentLevel::Finalized,
        );
        let result = poller.get_slots_to_process(100, 30).await;

        assert!(result.is_ok());
        let slots = result.unwrap();
        assert_eq!(slots.len(), 30);
        assert_eq!(slots[0], 100);
        assert_eq!(slots[29], 129);
    }

    #[tokio::test]
    async fn test_get_slots_to_process_already_caught_up() {
        let mut server = Server::new_async().await;
        let _m = mock_rpc_success(&mut server, "100").await;

        let poller = RpcPoller::new(
            server.url(),
            UiTransactionEncoding::Json,
            CommitmentLevel::Finalized,
        );
        let result = poller.get_slots_to_process(100, 30).await;

        assert!(result.is_ok());
        let slots = result.unwrap();
        assert!(slots.is_empty());
    }

    // ============================================================================
    // get_blocks_batch Tests
    // ============================================================================

    #[tokio::test]
    async fn test_get_blocks_batch_multiple_blocks() {
        let mut server = Server::new_async().await;

        // Mock expects 3 requests - mockito will match them in order
        let _m1 = mock_rpc_success(
            &mut server,
            r#"{
                "blockhash": "Block100",
                "parentSlot": 99,
                "transactions": []
            }"#,
        )
        .await
        .expect(1);

        let _m2 = mock_rpc_error(&mut server, -32009, "Slot was skipped")
            .await
            .expect(1);

        let _m3 = mock_rpc_success(
            &mut server,
            r#"{
                "blockhash": "Block102",
                "parentSlot": 101,
                "transactions": []
            }"#,
        )
        .await
        .expect(1);

        let poller = RpcPoller::new(
            server.url(),
            UiTransactionEncoding::Json,
            CommitmentLevel::Finalized,
        );
        let results = poller.get_blocks_batch(vec![100, 101, 102]).await;

        assert_eq!(results.len(), 3);

        // Slot 100: success
        assert_eq!(results[0].0, 100);
        assert!(results[0].1.is_ok());
        let block = results[0].1.as_ref().unwrap();
        assert!(block.is_some());
        assert_eq!(block.as_ref().unwrap().blockhash, "Block100");

        // Slot 101: skipped
        assert_eq!(results[1].0, 101);
        assert!(results[1].1.is_ok());
        assert!(results[1].1.as_ref().unwrap().is_none());

        // Slot 102: success
        assert_eq!(results[2].0, 102);
        assert!(results[2].1.is_ok());
        let block = results[2].1.as_ref().unwrap();
        assert!(block.is_some());
        assert_eq!(block.as_ref().unwrap().blockhash, "Block102");
    }

    #[tokio::test]
    async fn test_get_blocks_batch_empty() {
        let server = Server::new_async().await;
        let poller = RpcPoller::new(
            server.url(),
            UiTransactionEncoding::Json,
            CommitmentLevel::Finalized,
        );

        let results = poller.get_blocks_batch(vec![]).await;

        assert!(results.is_empty());
    }
}
