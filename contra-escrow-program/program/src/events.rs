extern crate alloc;

use alloc::vec::Vec;
use pinocchio::Address as Pubkey;
use shank::ShankType;

use crate::constants::EVENT_IX_TAG_LE;

#[repr(u8)]
pub enum EventDiscriminators {
    CreateInstance = 0,
    AllowMint = 1,
    BlockMint = 2,
    AddOperator = 3,
    RemoveOperator = 4,
    SetNewAdmin = 5,
    Deposit = 6,
    ReleaseFunds = 7,
    ResetSmtRoot = 8,
}

#[derive(ShankType)]
pub struct CreateInstanceEvent {
    /// Unique u8 byte for event type.
    pub event_discriminator: u8,
    /// Instance seed pubkey
    pub instance_seed: Pubkey,
    /// Admin pubkey
    pub admin: Pubkey,
}

impl CreateInstanceEvent {
    pub fn new(instance_seed: Pubkey, admin: Pubkey) -> Self {
        Self {
            event_discriminator: EventDiscriminators::CreateInstance as u8,
            instance_seed,
            admin,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut data = Vec::new();

        // Prepend IX Discriminator for emit_event.
        data.extend_from_slice(EVENT_IX_TAG_LE);
        data.push(self.event_discriminator);
        data.extend_from_slice(self.instance_seed.as_ref());
        data.extend_from_slice(self.admin.as_ref());

        data
    }
}

#[derive(ShankType)]
pub struct AllowMintEvent {
    /// Unique u8 byte for event type.
    pub event_discriminator: u8,
    /// Instance seed pubkey
    pub instance_seed: Pubkey,
    /// Mint pubkey that was allowed
    pub mint: Pubkey,
    /// Decimals of the mint (required to parse data on Contra)
    pub decimals: u8,
}

impl AllowMintEvent {
    pub fn new(instance_seed: Pubkey, mint: Pubkey, decimals: u8) -> Self {
        Self {
            event_discriminator: EventDiscriminators::AllowMint as u8,
            instance_seed,
            mint,
            decimals,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut data = Vec::new();

        // Prepend IX Discriminator for emit_event.
        data.extend_from_slice(EVENT_IX_TAG_LE);
        data.push(self.event_discriminator);
        data.extend_from_slice(self.instance_seed.as_ref());
        data.extend_from_slice(self.mint.as_ref());
        data.push(self.decimals);

        data
    }
}

#[derive(ShankType)]
pub struct BlockMintEvent {
    /// Unique u8 byte for event type.
    pub event_discriminator: u8,
    /// Instance seed pubkey
    pub instance_seed: Pubkey,
    /// Mint pubkey that was blocked
    pub mint: Pubkey,
}

impl BlockMintEvent {
    pub fn new(instance_seed: Pubkey, mint: Pubkey) -> Self {
        Self {
            event_discriminator: EventDiscriminators::BlockMint as u8,
            instance_seed,
            mint,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut data = Vec::new();

        // Prepend IX Discriminator for emit_event.
        data.extend_from_slice(EVENT_IX_TAG_LE);
        data.push(self.event_discriminator);
        data.extend_from_slice(self.instance_seed.as_ref());
        data.extend_from_slice(self.mint.as_ref());

        data
    }
}

#[derive(ShankType)]
pub struct AddOperatorEvent {
    /// Unique u8 byte for event type.
    pub event_discriminator: u8,
    /// Instance seed pubkey
    pub instance_seed: Pubkey,
    /// Operator pubkey that was granted operator access
    pub operator: Pubkey,
}

impl AddOperatorEvent {
    pub fn new(instance_seed: Pubkey, operator: Pubkey) -> Self {
        Self {
            event_discriminator: EventDiscriminators::AddOperator as u8,
            instance_seed,
            operator,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut data = Vec::new();

        // Prepend IX Discriminator for emit_event.
        data.extend_from_slice(EVENT_IX_TAG_LE);
        data.push(self.event_discriminator);
        data.extend_from_slice(self.instance_seed.as_ref());
        data.extend_from_slice(self.operator.as_ref());

        data
    }
}

#[derive(ShankType)]
pub struct RemoveOperatorEvent {
    /// Unique u8 byte for event type.
    pub event_discriminator: u8,
    /// Instance seed pubkey
    pub instance_seed: Pubkey,
    /// Operator pubkey that had operator access removed
    pub operator: Pubkey,
}

impl RemoveOperatorEvent {
    pub fn new(instance_seed: Pubkey, operator: Pubkey) -> Self {
        Self {
            event_discriminator: EventDiscriminators::RemoveOperator as u8,
            instance_seed,
            operator,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut data = Vec::new();

        // Prepend IX Discriminator for emit_event.
        data.extend_from_slice(EVENT_IX_TAG_LE);
        data.push(self.event_discriminator);
        data.extend_from_slice(self.instance_seed.as_ref());
        data.extend_from_slice(self.operator.as_ref());

        data
    }
}

#[derive(ShankType)]
pub struct SetNewAdminEvent {
    /// Unique u8 byte for event type.
    pub event_discriminator: u8,
    /// Instance seed pubkey
    pub instance_seed: Pubkey,
    /// Previous admin pubkey
    pub old_admin: Pubkey,
    /// New admin pubkey
    pub new_admin: Pubkey,
}

impl SetNewAdminEvent {
    pub fn new(instance_seed: Pubkey, old_admin: Pubkey, new_admin: Pubkey) -> Self {
        Self {
            event_discriminator: EventDiscriminators::SetNewAdmin as u8,
            instance_seed,
            old_admin,
            new_admin,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut data = Vec::new();

        // Prepend IX Discriminator for emit_event.
        data.extend_from_slice(EVENT_IX_TAG_LE);
        data.push(self.event_discriminator);
        data.extend_from_slice(self.instance_seed.as_ref());
        data.extend_from_slice(self.old_admin.as_ref());
        data.extend_from_slice(self.new_admin.as_ref());

        data
    }
}

#[derive(ShankType)]
pub struct DepositEvent {
    /// Unique u8 byte for event type.
    pub event_discriminator: u8,
    /// Instance seed pubkey
    pub instance_seed: Pubkey,
    /// User who made the deposit
    pub user: Pubkey,
    /// Amount of tokens deposited
    pub amount: u64,
    /// Recipient (for Contra tracking)
    pub recipient: Pubkey,
    /// Mint of the deposited tokens
    pub mint: Pubkey,
}

impl DepositEvent {
    pub fn new(
        instance_seed: Pubkey,
        user: Pubkey,
        amount: u64,
        recipient: Pubkey,
        mint: Pubkey,
    ) -> Self {
        Self {
            event_discriminator: EventDiscriminators::Deposit as u8,
            instance_seed,
            user,
            amount,
            recipient,
            mint,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut data = Vec::new();

        // Prepend IX Discriminator for emit_event.
        data.extend_from_slice(EVENT_IX_TAG_LE);
        data.push(self.event_discriminator);
        data.extend_from_slice(self.instance_seed.as_ref());
        data.extend_from_slice(self.user.as_ref());
        data.extend_from_slice(&self.amount.to_le_bytes());
        data.extend_from_slice(self.recipient.as_ref());
        data.extend_from_slice(self.mint.as_ref());

        data
    }
}

#[derive(ShankType)]
pub struct ReleaseFundsEvent {
    /// Unique u8 byte for event type.
    pub event_discriminator: u8,
    /// Instance seed pubkey
    pub instance_seed: Pubkey,
    /// Operator who released the funds
    pub operator: Pubkey,
    /// Amount of tokens released
    pub amount: u64,
    /// User receiving the funds
    pub user: Pubkey,
    /// Mint of the released tokens
    pub mint: Pubkey,
    /// New withdrawal transactions root after release
    pub new_withdrawal_root: [u8; 32],
}

impl ReleaseFundsEvent {
    pub fn new(
        instance_seed: Pubkey,
        operator: Pubkey,
        amount: u64,
        user: Pubkey,
        mint: Pubkey,
        new_withdrawal_root: [u8; 32],
    ) -> Self {
        Self {
            event_discriminator: EventDiscriminators::ReleaseFunds as u8,
            instance_seed,
            operator,
            amount,
            user,
            mint,
            new_withdrawal_root,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut data = Vec::new();

        // Prepend IX Discriminator for emit_event.
        data.extend_from_slice(EVENT_IX_TAG_LE);
        data.push(self.event_discriminator);
        data.extend_from_slice(self.instance_seed.as_ref());
        data.extend_from_slice(self.operator.as_ref());
        data.extend_from_slice(&self.amount.to_le_bytes());
        data.extend_from_slice(self.user.as_ref());
        data.extend_from_slice(self.mint.as_ref());
        data.extend_from_slice(&self.new_withdrawal_root);

        data
    }
}

#[derive(ShankType)]
pub struct ResetSmtRootEvent {
    /// Unique u8 byte for event type.
    pub event_discriminator: u8,
    /// Instance seed pubkey
    pub instance_seed: Pubkey,
    /// Operator who reset the SMT root
    pub operator: Pubkey,
}

impl ResetSmtRootEvent {
    pub fn new(instance_seed: Pubkey, operator: Pubkey) -> Self {
        Self {
            event_discriminator: EventDiscriminators::ResetSmtRoot as u8,
            instance_seed,
            operator,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut data = Vec::new();

        // Prepend IX Discriminator for emit_event.
        data.extend_from_slice(EVENT_IX_TAG_LE);
        data.push(self.event_discriminator);
        data.extend_from_slice(self.instance_seed.as_ref());
        data.extend_from_slice(self.operator.as_ref());

        data
    }
}
