extern crate alloc;

use pinocchio::Address as Pubkey;
use shank::ShankType;

const EVENT_SIZE: usize = size_of::<u64>() + size_of::<Pubkey>();

#[derive(ShankType)]
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
