extern crate alloc;
use super::discriminator::{AccountSerialize, PrivateChannelEscrowAccountDiscriminators, Discriminator};
use crate::constants::ALLOWED_MINT_SEED;
use crate::processor::validate_pda_account;
use crate::require_len;
use crate::validate_discriminator;
use crate::ID as PRIVATE_CHANNEL_ESCROW_PROGRAM_ID;
use alloc::vec;
use alloc::vec::Vec;
use codama::CodamaAccount;
use pinocchio::account::AccountView;
use pinocchio::{error::ProgramError, Address};

/// Seeds: [b"allowed_mint", instance_pda, mint_pubkey]
#[derive(Clone, Debug, PartialEq, CodamaAccount)]
#[repr(C)]
pub struct AllowedMint {
    pub bump: u8,
}

impl Discriminator for AllowedMint {
    const DISCRIMINATOR: u8 = PrivateChannelEscrowAccountDiscriminators::AllowedMintDiscriminator as u8;
}

impl AccountSerialize for AllowedMint {
    fn to_bytes_inner(&self) -> Vec<u8> {
        vec![self.bump]
    }
}

impl AllowedMint {
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
        mint: &Address,
        account_info: &AccountView,
    ) -> Result<(), ProgramError> {
        validate_pda_account(
            &[ALLOWED_MINT_SEED, instance_pda.as_ref(), mint.as_ref()],
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
    fn test_allowed_mint_new() {
        let allowed_mint = AllowedMint::new(99);
        assert_eq!(allowed_mint.bump, 99);
    }

    // Full serialize → deserialize cycle. Confirms the discriminator is written
    // as byte 0 and the bump is faithfully preserved across the wire format.
    #[test]
    fn test_allowed_mint_serialization_roundtrip() {
        let allowed_mint = AllowedMint::new(200);
        let bytes = allowed_mint.to_bytes();

        assert_eq!(bytes.len(), AllowedMint::LEN);
        assert_eq!(bytes[0], AllowedMint::DISCRIMINATOR);

        let deserialized = AllowedMint::try_from_bytes(&bytes).expect("Should deserialize");
        assert_eq!(deserialized.bump, allowed_mint.bump);
    }

    // Wrong discriminator byte should be rejected. Prevents a different account type
    // (e.g. Operator) from being accepted as an AllowedMint PDA, which would bypass
    // the mint allowlist entirely.
    #[test]
    fn test_allowed_mint_try_from_bytes_invalid_discriminator() {
        // Operator discriminator (1) placed where AllowedMint discriminator (2) is expected.
        let data = [1u8, 99u8];
        let result = AllowedMint::try_from_bytes(&data);
        assert_eq!(result.err(), Some(ProgramError::InvalidAccountData));
    }

    // Empty slice must be rejected — validate_discriminator! checks is_empty() first.
    #[test]
    fn test_allowed_mint_try_from_bytes_empty_data() {
        let result = AllowedMint::try_from_bytes(&[]);
        assert_eq!(result.err(), Some(ProgramError::InvalidAccountData));
    }

    // Correct discriminator but no bump byte — require_len! fires before the
    // field read, returning InvalidInstructionData instead of panicking.
    #[test]
    fn test_allowed_mint_try_from_bytes_too_short() {
        let data = [AllowedMint::DISCRIMINATOR]; // discriminator only, LEN=2 needs 2 bytes
        let result = AllowedMint::try_from_bytes(&data);
        assert_eq!(result.err(), Some(ProgramError::InvalidInstructionData));
    }
}
