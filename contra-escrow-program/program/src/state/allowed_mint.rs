extern crate alloc;
use super::discriminator::{AccountSerialize, ContraEscrowAccountDiscriminators, Discriminator};
use crate::constants::ALLOWED_MINT_SEED;
use crate::processor::validate_pda_account;
use crate::require_len;
use crate::validate_discriminator;
use crate::ID as CONTRA_ESCROW_PROGRAM_ID;
use alloc::vec;
use alloc::vec::Vec;
use pinocchio::account::AccountView;
use pinocchio::{error::ProgramError, Address};
use shank::ShankAccount;

/// Seeds: [b"allowed_mint", instance_pda, mint_pubkey]
#[derive(Clone, Debug, PartialEq, ShankAccount)]
#[repr(C)]
pub struct AllowedMint {
    pub bump: u8,
}

impl Discriminator for AllowedMint {
    const DISCRIMINATOR: u8 = ContraEscrowAccountDiscriminators::AllowedMintDiscriminator as u8;
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
            &CONTRA_ESCROW_PROGRAM_ID,
            self.bump,
            account_info,
        )
        .map(|_| ())
    }
}
