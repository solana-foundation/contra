#![allow(dead_code)]

use solana_sdk::pubkey::Pubkey;

pub const LOCAL_RPC_URL: &str = "http://localhost:18899";
pub const DEFAULT_INDEXER_DB_URL: &str = "postgres://contra:contra_password@localhost:5434/indexer";
pub const INDEXER_DB_ENV_VAR: &str = "TEST_INDEXER_DB_URL";

pub const NUM_USERS: usize = 5;
pub const DEPOSITS_PER_USER: usize = 10;
pub const BASE_AMOUNT: u64 = 10_000;
pub const WAIT_TIMEOUT_SECS: u64 = 120;

pub const ESCROW_INSTANCE_SEEDS_PRIVATE_KEY: [u8; 64] = [
    253, 137, 127, 96, 208, 56, 227, 155, 179, 196, 123, 197, 226, 86, 137, 104, 38, 0, 15, 229,
    175, 29, 110, 195, 49, 39, 28, 16, 184, 135, 196, 49, 46, 124, 200, 66, 209, 118, 114, 166,
    209, 41, 204, 119, 179, 128, 230, 85, 76, 156, 48, 21, 16, 75, 236, 76, 216, 53, 153, 60, 41,
    227, 56, 85,
];

#[derive(Debug, Clone)]
pub struct UserTransaction {
    pub user_pubkey: Pubkey,
    pub amount: u64,
    pub signature: String,
    pub slot: u64,
    pub tx_type: TransactionType,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TransactionType {
    Deposit,
    Withdrawal,
}
