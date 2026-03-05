extern crate alloc;
use super::discriminator::{AccountSerialize, ContraEscrowAccountDiscriminators, Discriminator};
use crate::constants::{
    tree_constants::{EMPTY_TREE_ROOT, MAX_TREE_LEAVES},
    INSTANCE_SEED, INSTANCE_VERSION,
};
use crate::error::ContraEscrowProgramError;
use crate::processor::validate_pda_account;
use crate::validate_discriminator;
use crate::ID as CONTRA_ESCROW_PROGRAM_ID;
use alloc::vec::Vec;
use pinocchio::account::AccountView;
use pinocchio::{error::ProgramError, Address as Pubkey};
use shank::ShankAccount;

/// Seeds: [b"instance", instance_seed.as_ref()]
#[derive(Clone, Debug, PartialEq, ShankAccount)]
#[repr(C)]
pub struct Instance {
    pub bump: u8,
    pub version: u8,
    pub instance_seed: Pubkey,
    pub admin: Pubkey,
    pub withdrawal_transactions_root: [u8; 32],

    // This is to prevent double spending across trees
    pub current_tree_index: u64,
}

impl Discriminator for Instance {
    const DISCRIMINATOR: u8 = ContraEscrowAccountDiscriminators::InstanceDiscriminator as u8;
}

impl AccountSerialize for Instance {
    fn to_bytes_inner(&self) -> Vec<u8> {
        let mut data = Vec::new();
        data.push(self.bump);
        data.push(self.version);
        data.extend_from_slice(self.instance_seed.as_ref());
        data.extend_from_slice(self.admin.as_ref());
        data.extend_from_slice(&self.withdrawal_transactions_root);
        data.extend_from_slice(&self.current_tree_index.to_le_bytes());
        data
    }
}

impl Instance {
    pub const LEN: usize = 1 + // discriminator
        1 + // bump
        1 + // version
        32 + // instance_seed
        32 + // admin
        32 + // withdrawal_transactions_root
        8; // current_tree_index

    pub fn new(bump: u8, instance_seed: Pubkey, admin: Pubkey) -> Self {
        Self {
            bump,
            version: INSTANCE_VERSION,
            instance_seed,
            admin,
            withdrawal_transactions_root: EMPTY_TREE_ROOT,
            current_tree_index: 0,
        }
    }

    pub fn validate_pda(&self, account_info: &AccountView) -> Result<(), ProgramError> {
        validate_pda_account(
            &[INSTANCE_SEED, self.instance_seed.as_ref()],
            &CONTRA_ESCROW_PROGRAM_ID,
            self.bump,
            account_info,
        )
        .map(|_| ())
    }

    pub fn validate_admin(&self, provided_admin: &Pubkey) -> Result<(), ProgramError> {
        if self.admin != *provided_admin {
            return Err(ContraEscrowProgramError::InvalidAdmin.into());
        }
        Ok(())
    }

    pub fn validate_current_tree_index(&self, transaction_nonce: u64) -> Result<(), ProgramError> {
        let expected_tree_index = transaction_nonce
            .checked_div(MAX_TREE_LEAVES as u64)
            .ok_or(ProgramError::ArithmeticOverflow)?;

        if self.current_tree_index != expected_tree_index {
            return Err(
                ContraEscrowProgramError::InvalidTransactionNonceForCurrentTreeIndex.into(),
            );
        }
        Ok(())
    }

    pub fn try_from_bytes(data: &[u8]) -> Result<Self, ProgramError> {
        validate_discriminator!(data, Self::DISCRIMINATOR);

        let mut offset: usize = 1;

        let bump = data[offset];
        offset += 1;

        let version = data[offset];
        offset += 1;

        let instance_seed = Pubkey::new_from_array(
            data[offset..offset + 32]
                .try_into()
                .map_err(|_| ProgramError::InvalidAccountData)?,
        );
        offset += 32;

        let admin = Pubkey::new_from_array(
            data[offset..offset + 32]
                .try_into()
                .map_err(|_| ProgramError::InvalidAccountData)?,
        );
        offset += 32;

        let mut withdrawal_transactions_root = [0u8; 32];
        withdrawal_transactions_root.copy_from_slice(&data[offset..offset + 32]);

        offset += 32;

        let current_tree_index = u64::from_le_bytes(
            data[offset..offset + 8]
                .try_into()
                .map_err(|_| ProgramError::InvalidInstructionData)?,
        );

        Ok(Self {
            bump,
            version,
            instance_seed,
            admin,
            withdrawal_transactions_root,
            current_tree_index,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_constructor() {
        let admin = Pubkey::new_from_array([0u8; 32]);
        let instance_seed = Pubkey::new_from_array([0u8; 32]);
        let instance = Instance::new(1, instance_seed, admin);

        assert_eq!(instance.version, INSTANCE_VERSION);
        assert_eq!(instance.admin, admin);
        assert_eq!(instance.instance_seed, instance_seed);
        assert_eq!(instance.withdrawal_transactions_root, EMPTY_TREE_ROOT);
    }
}
