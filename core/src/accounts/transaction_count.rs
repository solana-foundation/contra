/// Transaction count metadata value
///
/// Stores the total number of transactions that have been processed.
/// This is a standalone counter that persists even if the transactions table is truncated.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TransactionCount(u64);

impl TransactionCount {
    /// Create a new transaction count
    pub fn new(count: u64) -> Self {
        Self(count)
    }

    /// Get the count value
    pub fn count(&self) -> u64 {
        self.0
    }

    /// Increment the count by the given amount
    pub fn increment(&mut self, amount: u64) {
        self.0 = self.0.saturating_add(amount);
    }

    /// Serialize to bytes (little-endian u64)
    pub fn to_bytes(&self) -> [u8; 8] {
        self.0.to_le_bytes()
    }

    /// Deserialize from bytes (little-endian u64)
    ///
    /// Returns `None` if the byte slice is not exactly 8 bytes.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let byte_array: [u8; 8] = bytes.try_into().ok()?;
        Some(Self(u64::from_le_bytes(byte_array)))
    }
}

impl From<u64> for TransactionCount {
    fn from(count: u64) -> Self {
        Self(count)
    }
}

impl From<TransactionCount> for u64 {
    fn from(tc: TransactionCount) -> Self {
        tc.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serialization() {
        let count = TransactionCount::new(12345);
        let bytes = count.to_bytes();
        let deserialized = TransactionCount::from_bytes(&bytes).unwrap();
        assert_eq!(count, deserialized);
    }

    #[test]
    fn test_increment() {
        let mut count = TransactionCount::new(100);
        count.increment(50);
        assert_eq!(count.count(), 150);
    }

    #[test]
    fn test_saturating_add() {
        let mut count = TransactionCount::new(u64::MAX);
        count.increment(1);
        assert_eq!(count.count(), u64::MAX);
    }

    #[test]
    fn test_invalid_bytes_too_short() {
        let invalid = [1, 2, 3]; // Wrong length
        assert!(TransactionCount::from_bytes(&invalid).is_none());
    }

    #[test]
    fn test_invalid_bytes_too_long() {
        let invalid = [1, 2, 3, 4, 5, 6, 7, 8, 9]; // Too long
        assert!(TransactionCount::from_bytes(&invalid).is_none());
    }

    #[test]
    fn test_invalid_bytes_empty() {
        let empty: [u8; 0] = [];
        assert!(TransactionCount::from_bytes(&empty).is_none());
    }

    #[test]
    fn test_zero_bytes() {
        let zeros = [0u8; 8];
        let count = TransactionCount::from_bytes(&zeros).unwrap();
        assert_eq!(count.count(), 0);
    }

    #[test]
    fn test_max_value() {
        let count = TransactionCount::new(u64::MAX);
        let bytes = count.to_bytes();
        let deserialized = TransactionCount::from_bytes(&bytes).unwrap();
        assert_eq!(deserialized.count(), u64::MAX);
    }

    #[test]
    fn test_round_trip_various_values() {
        let test_values = [
            0,
            1,
            100,
            1000,
            1_000_000,
            u64::MAX / 2,
            u64::MAX - 1,
            u64::MAX,
        ];

        for value in test_values {
            let count = TransactionCount::new(value);
            let bytes = count.to_bytes();
            let deserialized = TransactionCount::from_bytes(&bytes).unwrap();
            assert_eq!(deserialized.count(), value, "Failed for value {}", value);
        }
    }
}
