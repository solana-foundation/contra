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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_discriminator_valid() {
        let result = ContraWithdrawInstructionDiscriminators::try_from(0u8);

        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            ContraWithdrawInstructionDiscriminators::WithdrawFunds
        );
    }

    #[test]
    fn test_discriminator_invalid() {
        let result = ContraWithdrawInstructionDiscriminators::try_from(1u8);

        assert!(result.is_err());
    }
}
