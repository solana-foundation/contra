extern crate alloc;
use super::discriminator::{AccountSerialize, PrivateChannelEscrowAccountDiscriminators, Discriminator};
use crate::constants::OPERATOR_SEED;
use crate::processor::validate_pda_account;
use crate::require_len;
use crate::validate_discriminator;
use crate::ID as PRIVATE_CHANNEL_ESCROW_PROGRAM_ID;
use alloc::vec;
use alloc::vec::Vec;
use codama::CodamaAccount;
use pinocchio::account::AccountView;
use pinocchio::{error::ProgramError, Address};

/// Seeds: [b"operator", instance_pda, wallet_pubkey]
#[derive(Clone, Debug, PartialEq, CodamaAccount)]
#[repr(C)]
pub struct Operator {
    pub bump: u8,
}

impl Discriminator for Operator {
    const DISCRIMINATOR: u8 = PrivateChannelEscrowAccountDiscriminators::OperatorDiscriminator as u8;
}

impl AccountSerialize for Operator {
    fn to_bytes_inner(&self) -> Vec<u8> {
        vec![self.bump]
    }
}

impl Operator {
    pub const LEN: usize = 1 + // discriminator
        1; // bump

    pub fn new(bump: u8) -> Self {
        Self { bump }
    }

    pub fn try_from_bytes(data: &[u8]) -> Result<Self, ProgramError> {
        validate_discriminator!(data, Self::DISCRIMINATOR);

        require_len!(data, Self::LEN);

        let offset: usize = 1;

        let bump = data[offset];

        Ok(Self { bump })
    }

    pub fn validate_pda(
        &self,
        instance_pda: &Address,
        wallet: &Address,
        account_info: &AccountView,
    ) -> Result<(), ProgramError> {
        validate_pda_account(
            &[OPERATOR_SEED, instance_pda.as_ref(), wallet.as_ref()],
            &PRIVATE_CHANNEL_ESCROW_PROGRAM_ID,
            self.bump,
            account_info,
        )
        .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Verifies the constructor stores the bump without modification.
    #[test]
    fn test_operator_new() {
        let operator = Operator::new(42);
        assert_eq!(operator.bump, 42);
    }

    // Full serialize → deserialize cycle. Guards against off-by-one errors in the
    // byte layout and confirms the discriminator is written as the first byte.
    #[test]
    fn test_operator_serialization_roundtrip() {
        let operator = Operator::new(200);
        let bytes = operator.to_bytes();

        assert_eq!(bytes.len(), Operator::LEN);
        assert_eq!(bytes[0], Operator::DISCRIMINATOR);

        let deserialized = Operator::try_from_bytes(&bytes).expect("Should deserialize");
        assert_eq!(deserialized.bump, operator.bump);
    }

    // Wrong discriminator byte should be rejected before any field is read.
    // Prevents a different account type (e.g. Instance) from being interpreted as
    // an Operator, which would allow spoofing operator permissions.
    #[test]
    fn test_operator_try_from_bytes_invalid_discriminator() {
        // Instance discriminator (0) placed where Operator discriminator (1) is expected.
        let data = [0u8, 42u8];
        let result = Operator::try_from_bytes(&data);
        assert_eq!(result.err(), Some(ProgramError::InvalidAccountData));
    }

    // Empty slice must be rejected — validate_discriminator! checks is_empty() first.
    #[test]
    fn test_operator_try_from_bytes_empty_data() {
        let result = Operator::try_from_bytes(&[]);
        assert_eq!(result.err(), Some(ProgramError::InvalidAccountData));
    }

    // Correct discriminator but no bump byte — require_len! fires before the
    // field read, returning InvalidInstructionData instead of panicking.
    #[test]
    fn test_operator_try_from_bytes_too_short() {
        let data = [Operator::DISCRIMINATOR]; // discriminator only, LEN=2 needs 2 bytes
        let result = Operator::try_from_bytes(&data);
        assert_eq!(result.err(), Some(ProgramError::InvalidInstructionData));
    }
}
