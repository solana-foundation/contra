use pinocchio::program_error::ProgramError;
use thiserror::Error;

/// Errors that may be returned by the Contra Withdraw Program.
#[repr(u32)]
#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum ContraWithdrawProgramError {
    /// (0) Invalid mint provided
    #[error("Invalid mint provided")]
    InvalidMint = 0,

    /// (1) Withdrawal amount must be greater than zero
    #[error("Withdrawal amount must be greater than zero")]
    ZeroAmount = 1,

    /// (2) Invalid event authority account
    #[error("Invalid event authority")]
    InvalidEventAuthority = 2,
}

impl From<ContraWithdrawProgramError> for ProgramError {
    fn from(e: ContraWithdrawProgramError) -> Self {
        ProgramError::Custom(e as u32)
    }
}
