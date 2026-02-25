use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Type;

/// Type of a transaction
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[sqlx(type_name = "transaction_type", rename_all = "lowercase")]
pub enum TransactionType {
    Deposit,
    Withdrawal,
}

/// Indexer state for checkpoint tracking
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct IndexerState {
    pub program_type: String,
    pub last_seen_slot: i64,
    pub updated_at: DateTime<Utc>,
}

/// Status of a transaction
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[sqlx(type_name = "transaction_status", rename_all = "lowercase")]
pub enum TransactionStatus {
    // Hasn't been picked up for processing by an Operator yet
    Pending,
    // Currently processing by an Operator
    Processing,
    Completed,
    Failed,
}

/// DbTransaction domain model
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
#[sqlx(type_name = "transaction", rename_all = "lowercase")]
pub struct DbTransaction {
    pub id: i64,
    pub signature: String,
    pub slot: i64,
    pub initiator: String,
    pub recipient: String,
    pub mint: String,
    pub amount: i64,
    pub memo: Option<String>,
    pub transaction_type: TransactionType,
    pub withdrawal_nonce: Option<i64>,
    pub status: TransactionStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub processed_at: Option<DateTime<Utc>>,
    // If this is a deposit from Solana to Contra, this will represent the Contra signature and
    // if this is a withdrawal from Contra to Solana, this will represent the Solana signature
    pub counterpart_signature: Option<String>,
}

/// Per-mint balance aggregate used during startup reconciliation.
/// Returned by the reconciliation storage query.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MintDbBalance {
    pub mint_address: String,
    pub token_program: String,
    /// Sum of amounts for all indexed deposits (any status).
    /// Deposits increase the on-chain ATA balance the moment they are observed,
    /// regardless of whether the operator has completed the corresponding contra mint.
    pub total_deposits: i64,
    /// Sum of amounts for completed withdrawals only.
    /// Only a completed `release_funds` call actually reduces the on-chain ATA balance.
    pub total_withdrawals: i64,
}

/// Mint metadata stored
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct DbMint {
    pub mint_address: String,
    pub decimals: i16,
    pub token_program: String,
    pub created_at: DateTime<Utc>,
}

impl DbMint {
    pub fn new(mint_address: String, decimals: i16, token_program: String) -> Self {
        Self {
            mint_address,
            decimals,
            token_program,
            created_at: Utc::now(),
        }
    }
}

/// Builder for DbTransaction
pub struct DbTransactionBuilder {
    signature: String,
    slot: i64,
    mint: String,
    amount: i64,
    initiator: Option<String>,
    recipient: Option<String>,
    memo: Option<String>,
    transaction_type: Option<TransactionType>,
}

impl DbTransactionBuilder {
    pub fn new(signature: String, slot: u64, mint: String, amount: u64) -> Self {
        Self {
            signature,
            slot: slot as i64,
            mint,
            amount: amount as i64,
            initiator: None,
            recipient: None,
            memo: None,
            transaction_type: None,
        }
    }

    pub fn initiator(mut self, initiator: String) -> Self {
        self.initiator = Some(initiator);
        self
    }

    pub fn recipient(mut self, recipient: String) -> Self {
        self.recipient = Some(recipient);
        self
    }

    pub fn memo(mut self, memo: Option<String>) -> Self {
        self.memo = memo;
        self
    }

    pub fn transaction_type(mut self, transaction_type: TransactionType) -> Self {
        self.transaction_type = Some(transaction_type);
        self
    }

    pub fn build(self) -> DbTransaction {
        let now = Utc::now();
        DbTransaction {
            id: 0,
            signature: self.signature,
            slot: self.slot,
            initiator: self.initiator.expect("initiator is required"),
            recipient: self.recipient.expect("recipient is required"),
            mint: self.mint,
            amount: self.amount,
            memo: self.memo,
            transaction_type: self.transaction_type.expect("transaction_type is required"),
            withdrawal_nonce: None,
            status: TransactionStatus::Pending,
            created_at: now,
            updated_at: now,
            processed_at: None,
            counterpart_signature: None,
        }
    }
}
