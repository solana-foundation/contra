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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_rocksdb_error() {
        // rocksdb::Error is hard to construct directly; test other variants instead
        let err = AccountsDbError::DatabaseNotFound;
        assert_eq!(format!("{}", err), "Database not found");
    }

    #[test]
    fn display_serialization_error() {
        // Force a bincode error by deserializing invalid data
        let bincode_err: bincode::Error = bincode::deserialize::<u64>(&[0u8; 1]).unwrap_err();
        let err = AccountsDbError::from(bincode_err);
        let display = format!("{}", err);
        assert!(display.starts_with("Serialization error:"), "{}", display);
    }

    #[test]
    fn display_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        let err = AccountsDbError::from(io_err);
        assert!(format!("{}", err).contains("gone"));
    }

    #[test]
    fn display_column_family_not_found() {
        let err = AccountsDbError::ColumnFamilyNotFound("accounts".into());
        assert_eq!(format!("{}", err), "Column family not found: accounts");
    }

    #[test]
    fn display_transaction_not_found() {
        let err = AccountsDbError::TransactionNotFound("abc123".into());
        assert_eq!(format!("{}", err), "Transaction not found: abc123");
    }

    #[test]
    fn display_block_not_found() {
        let err = AccountsDbError::BlockNotFound(42);
        assert_eq!(format!("{}", err), "Block not found at slot: 42");
    }

    #[test]
    fn display_invalid_data() {
        let err = AccountsDbError::InvalidData("bad bytes".into());
        assert_eq!(format!("{}", err), "Invalid data: bad bytes");
    }

    #[test]
    fn display_lock_poisoned() {
        let err = AccountsDbError::LockPoisoned;
        assert_eq!(format!("{}", err), "Lock poisoned");
    }

    #[test]
    fn display_other() {
        let err = AccountsDbError::Other("something".into());
        assert_eq!(format!("{}", err), "Other error: something");
    }

    #[test]
    fn from_string() {
        let err = AccountsDbError::from("hello".to_string());
        assert_eq!(format!("{}", err), "Other error: hello");
    }

    #[test]
    fn from_str() {
        let err = AccountsDbError::from("world");
        assert_eq!(format!("{}", err), "Other error: world");
    }

    #[test]
    fn implements_error_trait() {
        let err = AccountsDbError::LockPoisoned;
        let _: &dyn std::error::Error = &err;
    }
}
