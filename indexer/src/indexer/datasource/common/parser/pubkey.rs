use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

use crate::error::{account::AccountError, ParserError};

/// Helper: Parse a Pubkey from account keys
pub fn parse_pubkey(account_keys: &[String], index: usize) -> Result<Pubkey, ParserError> {
    let key_str = account_keys
        .get(index)
        .ok_or(AccountError::AccountIndexOutOfBounds { index })?;

    Pubkey::from_str(key_str).map_err(|e| ParserError::InvalidPubkey {
        reason: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use crate::test_utils::pubkey::test_pubkey;

    use super::*;

    // ============================================================================
    // parse_pubkey Tests
    // ============================================================================

    #[test]
    fn test_parse_pubkey_valid() {
        let pubkey = test_pubkey(42);
        let account_keys = vec![pubkey.to_string()];

        let result = parse_pubkey(&account_keys, 0);

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), pubkey);
    }

    #[test]
    fn test_parse_pubkey_invalid_base58() {
        let account_keys = vec!["not-a-valid-pubkey!!!".to_string()];

        let result = parse_pubkey(&account_keys, 0);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid pubkey"));
    }

    #[test]
    fn test_parse_pubkey_out_of_bounds() {
        let account_keys = vec![test_pubkey(1).to_string()];

        let result = parse_pubkey(&account_keys, 5);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("out of bounds"));
    }

    #[test]
    fn test_parse_pubkey_empty_array() {
        let account_keys: Vec<String> = vec![];

        let result = parse_pubkey(&account_keys, 0);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("out of bounds"));
    }

    #[test]
    fn test_parse_pubkey_first_element() {
        let pubkey1 = test_pubkey(10);
        let pubkey2 = test_pubkey(20);
        let account_keys = vec![pubkey1.to_string(), pubkey2.to_string()];

        let result = parse_pubkey(&account_keys, 0);

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), pubkey1);
    }
}
