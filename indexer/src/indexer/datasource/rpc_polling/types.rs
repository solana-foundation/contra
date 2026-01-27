use crate::indexer::datasource::common::types::CompiledInstruction;
use serde::Deserialize;

/// RPC block response types
#[derive(Debug, Deserialize, Clone)]
pub struct RpcBlock {
    pub blockhash: String,
    #[serde(rename = "parentSlot")]
    pub parent_slot: u64,
    pub transactions: Vec<RpcTransactionWithMeta>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RpcTransactionWithMeta {
    pub transaction: EncodedTransaction,
    pub meta: Option<TransactionMeta>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct EncodedTransaction {
    pub signatures: Vec<String>,
    pub message: EncodedMessage,
}

#[derive(Debug, Deserialize, Clone)]
pub struct EncodedMessage {
    #[serde(rename = "accountKeys")]
    pub account_keys: Vec<String>,
    pub instructions: Vec<CompiledInstruction>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct TransactionMeta {
    pub err: Option<serde_json::Value>,
    #[serde(rename = "logMessages")]
    pub log_messages: Option<Vec<String>>,
    #[serde(rename = "innerInstructions")]
    pub inner_instructions: Option<Vec<InnerInstructions>>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct InnerInstructions {
    pub index: u8,
    pub instructions: Vec<CompiledInstruction>,
}
