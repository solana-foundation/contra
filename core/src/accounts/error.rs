use std::{error::Error, fmt};

/// Custom error type for accounts database operations
#[derive(Debug)]
pub enum AccountsDbError {
    RocksDb(rocksdb::Error),
    Serialization(bincode::Error),
    Io(std::io::Error),
    DatabaseNotFound,
    ColumnFamilyNotFound(String),
    TransactionNotFound(String),
    BlockNotFound(u64),
    InvalidData(String),
    LockPoisoned,
    Other(String),
}

impl fmt::Display for AccountsDbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RocksDb(e) => write!(f, "RocksDB error: {}", e),
            Self::Serialization(e) => write!(f, "Serialization error: {}", e),
            Self::Io(e) => write!(f, "I/O error: {}", e),
            Self::DatabaseNotFound => write!(f, "Database not found"),
            Self::ColumnFamilyNotFound(name) => write!(f, "Column family not found: {}", name),
            Self::TransactionNotFound(sig) => write!(f, "Transaction not found: {}", sig),
            Self::BlockNotFound(slot) => write!(f, "Block not found at slot: {}", slot),
            Self::InvalidData(msg) => write!(f, "Invalid data: {}", msg),
            Self::LockPoisoned => write!(f, "Lock poisoned"),
            Self::Other(msg) => write!(f, "Other error: {}", msg),
        }
    }
}

impl Error for AccountsDbError {}

impl From<rocksdb::Error> for AccountsDbError {
    fn from(e: rocksdb::Error) -> Self {
        AccountsDbError::RocksDb(e)
    }
}

impl From<bincode::Error> for AccountsDbError {
    fn from(e: bincode::Error) -> Self {
        AccountsDbError::Serialization(e)
    }
}

impl From<std::io::Error> for AccountsDbError {
    fn from(e: std::io::Error) -> Self {
        AccountsDbError::Io(e)
    }
}

impl From<String> for AccountsDbError {
    fn from(s: String) -> Self {
        AccountsDbError::Other(s)
    }
}

impl From<&str> for AccountsDbError {
    fn from(s: &str) -> Self {
        AccountsDbError::Other(s.to_string())
    }
}

/// Result type for accounts database operations
pub type AccountsDbResult<T> = Result<T, AccountsDbError>;
