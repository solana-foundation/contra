extern crate alloc;

use alloc::vec::Vec;
use pinocchio::pubkey::Pubkey;
use shank::ShankType;

#[derive(ShankType)]
pub struct WithdrawFundsEvent {
    /// Amount withdrawn
    pub amount: u64,
    /// Destination pubkey
    pub destination: Pubkey,
}

impl WithdrawFundsEvent {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut data = Vec::new();

        data.extend_from_slice(&self.amount.to_le_bytes());
        data.extend_from_slice(self.destination.as_ref());

        data
    }
}
