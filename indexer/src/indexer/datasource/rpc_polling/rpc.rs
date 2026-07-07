use crate::error::DataSourceRpcError;
use crate::indexer::datasource::rpc_polling::types::{BlockFetch, RpcBlock};
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

    /// Fetch just the getBlock result: `Some(block)` when present, `None` for a
    /// `-32004/-32007/-32009/null` "skipped or missing" response (which the node
    /// cannot tell apart from unavailable). Other RPC errors surface as `Err`.
    async fn fetch_block_raw(&self, slot: u64) -> Result<Option<RpcBlock>, DataSourceRpcError> {
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
                        "rewards": false,
                        "commitment": self.commitment.to_string(),
                    }
                ]
            }))
            .send()
            .await?;

        let json: serde_json::Value = response.json().await?;

        if let Some(error) = json.get("error") {
            // -32004: Block not available for slot (skipped or missing)
            // -32007: Slot skipped or missing due to ledger jump to recent snapshot
            // -32009: Slot skipped or missing in long-term storage
            if error["code"] == -32004 || error["code"] == -32007 || error["code"] == -32009 {
                return Ok(None);
            }
            return Err(DataSourceRpcError::Protocol {
                reason: format!("RPC error: {}", error),
            });
        }

        // A null result is the same "skipped or missing" ambiguity as the errors above.
        if json["result"].is_null() {
            return Ok(None);
        }

        let block: RpcBlock =
            serde_json::from_value(json["result"].clone()).map_err(DataSourceRpcError::from)?;
        Ok(Some(block))
    }

    /// Classify a "skipped or missing" slot against the node's retained ledger
    /// floor: at or above `floor` it is a proven skip (safe to advance), below it
    /// the block was pruned and its contents are unknown. Inclusive because `floor`
    /// is itself a retained slot. Assumes the getBlock and getFirstAvailableBlock
    /// calls observe a consistent node view; a load-balanced endpoint split across
    /// lagging replicas can still misclassify.
    fn classify_absent(slot: u64, floor: u64) -> BlockFetch {
        if slot >= floor {
            BlockFetch::Skipped
        } else {
            BlockFetch::Unavailable
        }
    }

    /// Get a single block by slot, classifying an absent slot with its own floor
    /// query. Only the batch path runs in production; this is the single-slot form
    /// the unit tests drive.
    #[cfg(test)]
    async fn get_block(&self, slot: u64) -> Result<BlockFetch, DataSourceRpcError> {
        match self.fetch_block_raw(slot).await? {
            Some(block) => Ok(BlockFetch::Present(block)),
            None => Ok(Self::classify_absent(
                slot,
                self.get_first_available_block().await?,
            )),
        }
    }

    /// Get multiple blocks in parallel. The ledger floor is queried at most once
    /// per batch, and only when some slot is absent, so a fully-present batch adds
    /// no extra round-trips and N absent slots do not each re-query the floor.
    pub async fn get_blocks_batch(
        &self,
        slots: Vec<u64>,
    ) -> Vec<(u64, Result<BlockFetch, DataSourceRpcError>)> {
        let raws = join_all(
            slots
                .into_iter()
                .map(|slot| async move { (slot, self.fetch_block_raw(slot).await) }),
        )
        .await;

        let floor = if raws.iter().any(|(_, r)| matches!(r, Ok(None))) {
            Some(self.get_first_available_block().await)
        } else {
            None
        };

        raws.into_iter()
            .map(|(slot, raw)| {
                let result = match raw {
                    Ok(Some(block)) => Ok(BlockFetch::Present(block)),
                    Ok(None) => match &floor {
                        Some(Ok(f)) => Ok(Self::classify_absent(slot, *f)),
                        // The single floor query failed, so no absent slot in this
                        // batch can be classified; fail closed for each.
                        Some(Err(e)) => Err(DataSourceRpcError::Protocol {
                            reason: format!("getFirstAvailableBlock failed: {e}"),
                        }),
                        None => unreachable!("floor is queried whenever a slot is absent"),
                    },
                    Err(e) => Err(e),
                };
                (slot, result)
            })
            .collect()
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

    /// Get the first slot the node still retains (its ledger floor). Takes no
    /// params and returns node-local ledger state, so no commitment argument.
    pub async fn get_first_available_block(&self) -> Result<u64, DataSourceRpcError> {
        let response = self
            .client
            .post(&self.rpc_url)
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "getFirstAvailableBlock",
                "params": []
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
                reason: "Invalid getFirstAvailableBlock response".to_string(),
            })
    }

    /// Get slot range to process, returning (slots, chain_tip)
    pub async fn get_slots_to_process(
        &self,
        from_slot: u64,
        max_slots: usize,
    ) -> Result<(Vec<u64>, u64), DataSourceRpcError> {
        let latest_slot = self.get_latest_slot().await?;

        if from_slot >= latest_slot {
            return Ok((vec![], latest_slot));
        }

        let to_slot = std::cmp::min(from_slot + max_slots as u64, latest_slot);
        Ok(((from_slot..to_slot).collect(), latest_slot))
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
        let block = match result.unwrap() {
            BlockFetch::Present(block) => block,
            other => panic!("expected Present, got {other:?}"),
        };
        assert_eq!(
            block.blockhash,
            "TestBlockHash11111111111111111111111111111"
        );
        assert_eq!(block.parent_slot, 100);
    }

    /// -32009 at or above the retained floor is a proven skip, safe to advance.
    #[tokio::test]
    async fn get_block_skipped_when_slot_at_or_above_floor() {
        let mut server = Server::new_async().await;
        let _m = mock_rpc_error(&mut server, -32009, "Slot was skipped").await;
        let _floor = mock_first_available_block(&mut server, 50);

        let poller = RpcPoller::new(
            server.url(),
            UiTransactionEncoding::Json,
            CommitmentLevel::Finalized,
        );
        let result = poller.get_block(101).await;

        assert!(matches!(result, Ok(BlockFetch::Skipped)));
    }

    /// -32009 below the retained floor is pruned history: Unavailable, must not advance.
    #[tokio::test]
    async fn get_block_unavailable_when_slot_below_floor() {
        let mut server = Server::new_async().await;
        let _m = mock_rpc_error(&mut server, -32009, "Slot missing in long-term storage").await;
        let _floor = mock_first_available_block(&mut server, 50);

        let poller = RpcPoller::new(
            server.url(),
            UiTransactionEncoding::Json,
            CommitmentLevel::Finalized,
        );
        let result = poller.get_block(40).await;

        assert!(matches!(result, Ok(BlockFetch::Unavailable)));
    }

    /// A null result takes the same floor-classified path; below floor is Unavailable.
    #[tokio::test]
    async fn get_block_null_result_classified_by_floor() {
        let mut server = Server::new_async().await;
        let _m = mock_rpc_success(&mut server, "null").await;
        let _floor = mock_first_available_block(&mut server, 50);

        let poller = RpcPoller::new(
            server.url(),
            UiTransactionEncoding::Json,
            CommitmentLevel::Finalized,
        );
        let result = poller.get_block(40).await;

        assert!(matches!(result, Ok(BlockFetch::Unavailable)));
    }

    /// The `slot >= floor` boundary is inclusive: slot exactly at the floor is Skipped.
    #[tokio::test]
    async fn get_block_floor_boundary_inclusive() {
        let mut server = Server::new_async().await;
        let _m = mock_rpc_error(&mut server, -32007, "Slot skipped or ledger jump").await;
        let _floor = mock_first_available_block(&mut server, 50);

        let poller = RpcPoller::new(
            server.url(),
            UiTransactionEncoding::Json,
            CommitmentLevel::Finalized,
        );
        let result = poller.get_block(50).await;

        assert!(matches!(result, Ok(BlockFetch::Skipped)));
    }

    /// A failed floor fetch is transient and retryable, so it propagates as Err,
    /// never as a proven-unavailable verdict.
    #[tokio::test]
    async fn get_block_floor_fetch_error_propagates_err() {
        let mut server = Server::new_async().await;
        let _m = mock_rpc_error(&mut server, -32009, "Slot was skipped").await;
        // Floor call errors; body-matched so it only answers getFirstAvailableBlock.
        let _floor_err = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(json!({
                "method": "getFirstAvailableBlock"
            })))
            .with_status(200)
            .with_body(
                json!({
                    "jsonrpc": "2.0",
                    "error": { "code": -32600, "message": "Invalid request" },
                    "id": 1
                })
                .to_string(),
            )
            .create();

        let poller = RpcPoller::new(
            server.url(),
            UiTransactionEncoding::Json,
            CommitmentLevel::Finalized,
        );
        let result = poller.get_block(40).await;

        assert!(result.is_err());
    }

    /// -32004 "Block not available for slot" is the error-object spelling of an
    /// absent slot; at or above the retained floor it is a proven skip.
    #[tokio::test]
    async fn get_block_not_available_classified_by_floor() {
        let mut server = Server::new_async().await;
        let _m = mock_rpc_error(&mut server, -32004, "Block not available for slot 101").await;
        let _floor = mock_first_available_block(&mut server, 50);

        let poller = RpcPoller::new(
            server.url(),
            UiTransactionEncoding::Json,
            CommitmentLevel::Finalized,
        );
        let result = poller.get_block(101).await;

        assert!(matches!(result.unwrap(), BlockFetch::Skipped));
    }

    #[tokio::test]
    async fn get_first_available_block_success() {
        let mut server = Server::new_async().await;
        let _m = mock_rpc_success(&mut server, "42").await;

        let poller = RpcPoller::new(
            server.url(),
            UiTransactionEncoding::Json,
            CommitmentLevel::Finalized,
        );
        let result = poller.get_first_available_block().await;

        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn get_first_available_block_rpc_error() {
        let mut server = Server::new_async().await;
        let _m = mock_rpc_error(&mut server, -32600, "Invalid request").await;

        let poller = RpcPoller::new(
            server.url(),
            UiTransactionEncoding::Json,
            CommitmentLevel::Finalized,
        );
        let result = poller.get_first_available_block().await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("RPC error"));
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

    /// Regression: `get_block` must forward the configured commitment in the
    /// request body. A previous version omitted the field, so the server
    /// defaulted to `finalized` regardless of `RpcPoller::new`'s argument —
    /// causing `solana-test-validator` to reply `-32009` for slots that were
    /// only confirmed.
    #[tokio::test]
    async fn test_get_block_forwards_configured_commitment() {
        let mut server = Server::new_async().await;
        let m = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(json!({
                "method": "getBlock",
                "params": [
                    101,
                    { "commitment": "confirmed" }
                ]
            })))
            .with_status(200)
            .with_body(r#"{"jsonrpc":"2.0","result":null,"id":1}"#)
            .expect(1)
            .create_async()
            .await;
        // The null result routes through the floor check; slot 101 >= 0 is a proven skip.
        let _floor = mock_first_available_block(&mut server, 0);

        let poller = RpcPoller::new(
            server.url(),
            UiTransactionEncoding::Json,
            CommitmentLevel::Confirmed,
        );
        let result = poller.get_block(101).await;

        m.assert_async().await;
        assert!(matches!(result.unwrap(), BlockFetch::Skipped));
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
        let (slots, chain_tip) = result.unwrap();
        assert_eq!(slots.len(), 30);
        assert_eq!(slots[0], 100);
        assert_eq!(slots[29], 129);
        assert_eq!(chain_tip, 150);
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
        let (slots, chain_tip) = result.unwrap();
        assert!(slots.is_empty());
        assert_eq!(chain_tip, 100);
    }

    // ============================================================================
    // get_blocks_batch Tests
    // ============================================================================

    #[tokio::test]
    async fn test_get_blocks_batch_multiple_blocks() {
        let mut server = Server::new_async().await;

        // Use body matchers so each mock responds to its specific slot,
        // regardless of the order concurrent requests arrive.
        let _m1 = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(json!({
                "method": "getBlock",
                "params": [100]
            })))
            .with_status(200)
            .with_body(
                json!({
                    "jsonrpc": "2.0",
                    "result": {
                        "blockhash": "Block100",
                        "parentSlot": 99,
                        "transactions": []
                    },
                    "id": 1
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;

        let _m2 = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(json!({
                "method": "getBlock",
                "params": [101]
            })))
            .with_status(200)
            .with_body(
                json!({
                    "jsonrpc": "2.0",
                    "error": { "code": -32009, "message": "Slot was skipped" },
                    "id": 1
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;

        let _m3 = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(json!({
                "method": "getBlock",
                "params": [102]
            })))
            .with_status(200)
            .with_body(
                json!({
                    "jsonrpc": "2.0",
                    "result": {
                        "blockhash": "Block102",
                        "parentSlot": 101,
                        "transactions": []
                    },
                    "id": 1
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;

        // Floor 100 so slot 101's -32009 (101 >= 100) classifies as Skipped.
        let _floor = mock_first_available_block(&mut server, 100);

        let poller = RpcPoller::new(
            server.url(),
            UiTransactionEncoding::Json,
            CommitmentLevel::Finalized,
        );
        let results = poller.get_blocks_batch(vec![100, 101, 102]).await;

        assert_eq!(results.len(), 3);

        // Slot 100: present
        assert_eq!(results[0].0, 100);
        match results[0].1.as_ref().unwrap() {
            BlockFetch::Present(block) => assert_eq!(block.blockhash, "Block100"),
            other => panic!("slot 100 expected Present, got {other:?}"),
        }

        // Slot 101: proven skipped (>= floor)
        assert_eq!(results[1].0, 101);
        assert!(matches!(
            results[1].1.as_ref().unwrap(),
            BlockFetch::Skipped
        ));

        // Slot 102: present
        assert_eq!(results[2].0, 102);
        match results[2].1.as_ref().unwrap() {
            BlockFetch::Present(block) => assert_eq!(block.blockhash, "Block102"),
            other => panic!("slot 102 expected Present, got {other:?}"),
        }
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
