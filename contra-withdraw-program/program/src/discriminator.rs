#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContraWithdrawInstructionDiscriminators {
    WithdrawFunds = 0,
}

impl TryFrom<u8> for ContraWithdrawInstructionDiscriminators {
    type Error = ();

    fn try_from(discriminator: u8) -> Result<Self, Self::Error> {
        match discriminator {
            0 => Ok(Self::WithdrawFunds),
            _ => Err(()),
        }
    }
}
