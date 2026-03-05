extern crate alloc;

use codama::CodamaType;
use pinocchio::Address as Pubkey;

const EVENT_SIZE: usize = size_of::<u64>() + size_of::<Pubkey>();

#[derive(CodamaType)]
pub struct WithdrawFundsEvent {
    pub amount: u64,
    pub destination: Pubkey,
}

impl WithdrawFundsEvent {
    pub fn to_bytes(&self) -> [u8; EVENT_SIZE] {
        let mut data = [0u8; EVENT_SIZE];
        data[..8].copy_from_slice(&self.amount.to_le_bytes());
        data[8..].copy_from_slice(self.destination.as_ref());
        data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_withdraw_funds_event_to_bytes() {
        let destination = Pubkey::new_from_array([0u8; 32]);
        let amount = 1000u64;

        let event = WithdrawFundsEvent {
            amount,
            destination,
        };

        let bytes = event.to_bytes();

        assert_eq!(bytes.len(), 40);
        assert_eq!(&bytes[..8], &amount.to_le_bytes());
        assert_eq!(&bytes[8..], destination.as_ref());
    }
}
