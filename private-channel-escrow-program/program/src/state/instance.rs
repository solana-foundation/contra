extern crate alloc;
use super::discriminator::{
    AccountSerialize, Discriminator, PrivateChannelEscrowAccountDiscriminators,
};
use crate::constants::{
    tree_constants::{EMPTY_TREE_ROOT, MAX_TREE_LEAVES},
    INSTANCE_SEED, INSTANCE_VERSION,
};
use crate::error::PrivateChannelEscrowProgramError;
use crate::processor::validate_pda_account;
use crate::validate_discriminator;
use crate::ID as PRIVATE_CHANNEL_ESCROW_PROGRAM_ID;
use alloc::vec::Vec;
use codama::CodamaAccount;
use pinocchio::account::AccountView;
use pinocchio::{error::ProgramError, Address as Pubkey};

/// Seeds: [b"instance", instance_seed.as_ref()]
#[derive(Clone, Debug, PartialEq, CodamaAccount)]
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
    const DISCRIMINATOR: u8 =
        PrivateChannelEscrowAccountDiscriminators::InstanceDiscriminator as u8;
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
            &PRIVATE_CHANNEL_ESCROW_PROGRAM_ID,
            self.bump,
            account_info,
        )
        .map(|_| ())
    }

    pub fn validate_admin(&self, provided_admin: &Pubkey) -> Result<(), ProgramError> {
        if self.admin != *provided_admin {
            return Err(PrivateChannelEscrowProgramError::InvalidAdmin.into());
        }
        Ok(())
    }

    pub fn validate_current_tree_index(&self, transaction_nonce: u64) -> Result<(), ProgramError> {
        let expected_tree_index = transaction_nonce
            .checked_div(MAX_TREE_LEAVES as u64)
            .ok_or(ProgramError::ArithmeticOverflow)?;

        if self.current_tree_index != expected_tree_index {
            return Err(
                PrivateChannelEscrowProgramError::InvalidTransactionNonceForCurrentTreeIndex.into(),
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

    #[test]
    fn test_checked_add_overflow_tree_index() {
        // Verify that checked_add on u64::MAX returns None (would cause ArithmeticOverflow)
        let admin = Pubkey::new_from_array([0u8; 32]);
        let instance_seed = Pubkey::new_from_array([0u8; 32]);
        let mut instance = Instance::new(1, instance_seed, admin);

        // Simulate tree_index at u64::MAX
        instance.current_tree_index = u64::MAX;

        // checked_add(1) should return None, which the processor maps to ArithmeticOverflow
        let result = instance.current_tree_index.checked_add(1);
        assert_eq!(
            result, None,
            "checked_add(1) on u64::MAX should return None (overflow)"
        );
    }

    #[test]
    fn test_validate_current_tree_index_nonce_zero() {
        let admin = Pubkey::new_from_array([0u8; 32]);
        let instance_seed = Pubkey::new_from_array([0u8; 32]);
        let instance = Instance::new(1, instance_seed, admin);

        // tree_index=0, nonce=0 -> expected_tree_index = 0/65536 = 0. Should pass.
        let result = instance.validate_current_tree_index(0);
        assert!(result.is_ok(), "Nonce 0 should be valid for tree_index 0");
    }

    #[test]
    fn test_validate_current_tree_index_boundary() {
        let admin = Pubkey::new_from_array([0u8; 32]);
        let instance_seed = Pubkey::new_from_array([0u8; 32]);
        let instance = Instance::new(1, instance_seed, admin);

        // tree_index=0: valid nonces are 0..65535
        // nonce=65535 -> expected_tree_index = 65535/65536 = 0. Should pass.
        let result = instance.validate_current_tree_index(MAX_TREE_LEAVES as u64 - 1);
        assert!(
            result.is_ok(),
            "Last nonce in tree_index=0 range should be valid"
        );

        // nonce=65536 -> expected_tree_index = 65536/65536 = 1. Should fail for tree_index=0.
        let result = instance.validate_current_tree_index(MAX_TREE_LEAVES as u64);
        assert!(
            result.is_err(),
            "First nonce of tree_index=1 should be invalid for tree_index=0"
        );
    }

    #[test]
    fn test_serialization_roundtrip() {
        let admin = Pubkey::new_from_array([42u8; 32]);
        let instance_seed = Pubkey::new_from_array([7u8; 32]);
        let mut instance = Instance::new(255, instance_seed, admin);
        instance.current_tree_index = 12345;
        instance.withdrawal_transactions_root = [99u8; 32];

        let bytes = instance.to_bytes();
        let deserialized = Instance::try_from_bytes(&bytes).expect("Should deserialize");

        assert_eq!(deserialized.bump, instance.bump);
        assert_eq!(deserialized.version, instance.version);
        assert_eq!(deserialized.instance_seed, instance.instance_seed);
        assert_eq!(deserialized.admin, instance.admin);
        assert_eq!(
            deserialized.withdrawal_transactions_root,
            instance.withdrawal_transactions_root
        );
        assert_eq!(deserialized.current_tree_index, instance.current_tree_index);
    }

    // validate_admin should return Ok when the provided key matches the stored admin.
    #[test]
    fn test_validate_admin_correct() {
        let admin = Pubkey::new_from_array([5u8; 32]);
        let instance = Instance::new(1, Pubkey::new_from_array([1u8; 32]), admin);

        assert!(instance.validate_admin(&admin).is_ok());
    }

    // validate_admin should return InvalidAdmin when a different key is provided.
    // This is the guard that prevents a non-admin from performing admin-only operations
    // (allow_mint, block_mint, add_operator, remove_operator, set_new_admin).
    #[test]
    fn test_validate_admin_wrong() {
        let admin = Pubkey::new_from_array([5u8; 32]);
        let wrong_admin = Pubkey::new_from_array([6u8; 32]);
        let instance = Instance::new(1, Pubkey::new_from_array([1u8; 32]), admin);

        let result = instance.validate_admin(&wrong_admin);

        assert_eq!(
            result.err(),
            Some(PrivateChannelEscrowProgramError::InvalidAdmin.into())
        );
    }

    // try_from_bytes should reject data whose first byte doesn't match the Instance
    // discriminator, preventing a different account type from being parsed as an Instance.
    #[test]
    fn test_try_from_bytes_invalid_discriminator() {
        let admin = Pubkey::new_from_array([42u8; 32]);
        let instance_seed = Pubkey::new_from_array([7u8; 32]);
        let instance = Instance::new(1, instance_seed, admin);
        let mut bytes = instance.to_bytes();
        bytes[0] = 99; // corrupt the discriminator byte

        let result = Instance::try_from_bytes(&bytes);

        assert_eq!(result.err(), Some(ProgramError::InvalidAccountData));
    }

    // Verifies tree index 1 behaviour: nonces belonging to tree 1 (>= MAX_TREE_LEAVES)
    // are valid, while nonces from tree 0 are rejected. This ensures the math holds after the first reset.
    #[test]
    fn test_validate_current_tree_index_second_tree() {
        let mut instance = Instance::new(
            1,
            Pubkey::new_from_array([0u8; 32]),
            Pubkey::new_from_array([0u8; 32]),
        );
        instance.current_tree_index = 1;

        // First nonce of tree 1 — expected_tree_index = MAX_TREE_LEAVES / MAX_TREE_LEAVES = 1.
        assert!(instance
            .validate_current_tree_index(MAX_TREE_LEAVES as u64)
            .is_ok());

        // Last nonce of tree 0 — expected_tree_index = 0, should fail for tree 1.
        assert!(instance
            .validate_current_tree_index(MAX_TREE_LEAVES as u64 - 1)
            .is_err());
    }
}
