extern crate alloc;
use super::discriminator::{AccountSerialize, ContraEscrowAccountDiscriminators, Discriminator};
use crate::constants::OPERATOR_SEED;
use crate::processor::validate_pda_account;
use crate::require_len;
use crate::validate_discriminator;
use crate::ID as CONTRA_ESCROW_PROGRAM_ID;
use alloc::vec;
use alloc::vec::Vec;
use pinocchio::account::AccountView;
use pinocchio::{error::ProgramError, Address};
use shank::ShankAccount;

/// Seeds: [b"operator", instance_pda, wallet_pubkey]
#[derive(Clone, Debug, PartialEq, ShankAccount)]
#[repr(C)]
pub struct Operator {
    pub bump: u8,
}

impl Discriminator for Operator {
    const DISCRIMINATOR: u8 = ContraEscrowAccountDiscriminators::OperatorDiscriminator as u8;
}

impl AccountSerialize for Operator {
    fn to_bytes_inner(&self) -> Vec<u8> {
        vec![self.bump]
    }
}

impl Operator {
    pub const LEN: usize = 1 + // discriminator
        1; // bump

    pub fn new(bump: u8) -> Result<Self, ProgramError> {
        Ok(Self { bump })
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
            &CONTRA_ESCROW_PROGRAM_ID,
            self.bump,
            account_info,
        )?;
        Ok(())
    }
}
