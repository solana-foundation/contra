extern crate alloc;

use alloc::vec::Vec;

pub trait Discriminator {
    const DISCRIMINATOR: u8;
}

#[repr(u8)]
pub enum ContraEscrowAccountDiscriminators {
    InstanceDiscriminator = 0,
    OperatorDiscriminator = 1,
    AllowedMintDiscriminator = 2,
}

#[repr(u8)]
pub enum ContraEscrowInstructionDiscriminators {
    CreateInstance = 0,
    AllowMint = 1,
    BlockMint = 2,
    AddOperator = 3,
    RemoveOperator = 4,
    SetNewAdmin = 5,
    Deposit = 6,
    ReleaseFunds = 7,
    ResetSmtRoot = 8,
    EmitEvent = 228,
}

#[repr(u8)]
pub enum ContraEscrowEventDiscriminators {
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

impl TryFrom<u8> for ContraEscrowInstructionDiscriminators {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(ContraEscrowInstructionDiscriminators::CreateInstance),
            1 => Ok(ContraEscrowInstructionDiscriminators::AllowMint),
            2 => Ok(ContraEscrowInstructionDiscriminators::BlockMint),
            3 => Ok(ContraEscrowInstructionDiscriminators::AddOperator),
            4 => Ok(ContraEscrowInstructionDiscriminators::RemoveOperator),
            5 => Ok(ContraEscrowInstructionDiscriminators::SetNewAdmin),
            6 => Ok(ContraEscrowInstructionDiscriminators::Deposit),
            7 => Ok(ContraEscrowInstructionDiscriminators::ReleaseFunds),
            8 => Ok(ContraEscrowInstructionDiscriminators::ResetSmtRoot),
            228 => Ok(ContraEscrowInstructionDiscriminators::EmitEvent),
            _ => Err(()),
        }
    }
}

pub trait AccountSerialize: Discriminator {
    fn to_bytes_inner(&self) -> Vec<u8>;

    fn to_bytes(&self) -> Vec<u8> {
        let mut data = Vec::new();
        data.push(Self::DISCRIMINATOR);
        data.extend_from_slice(&self.to_bytes_inner());
        data
    }
}
