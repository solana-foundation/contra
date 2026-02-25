extern crate alloc;

use alloc::vec::Vec;
use pinocchio::pubkey::Pubkey;
use shank::ShankType;

use crate::constants::EVENT_IX_TAG_LE;

#[repr(u8)]
pub enum EventDiscriminators {
    WithdrawFunds = 0,
}

#[derive(ShankType)]
pub struct WithdrawFundsEvent {
    /// Unique u8 byte for event type.
    pub event_discriminator: u8,
    /// Amount withdrawn
    pub amount: u64,
    /// Destination pubkey
    pub destination: Pubkey,
}

impl WithdrawFundsEvent {
    pub fn new(amount: u64, destination: Pubkey) -> Self {
        Self {
            event_discriminator: EventDiscriminators::WithdrawFunds as u8,
            amount,
            destination,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut data = Vec::new();

        data.extend_from_slice(EVENT_IX_TAG_LE);
        data.push(self.event_discriminator);
        data.extend_from_slice(&self.amount.to_le_bytes());
        data.extend_from_slice(self.destination.as_ref());

        data
    }
}
