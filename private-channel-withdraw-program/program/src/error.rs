use codama::CodamaErrors;
use pinocchio::error::ProgramError;
use thiserror::Error;

/// Errors that may be returned by the PrivateChannel Withdraw Program.
#[derive(Clone, Debug, Eq, PartialEq, Error, CodamaErrors)]
pub enum PrivateChannelWithdrawProgramError {
    /// (0) Invalid mint provided
    #[error("Invalid mint provided")]
    InvalidMint,

    /// (1) Withdrawal amount must be greater than zero
    #[error("Withdrawal amount must be greater than zero")]
    ZeroAmount,
}

impl From<PrivateChannelWithdrawProgramError> for ProgramError {
    fn from(e: PrivateChannelWithdrawProgramError) -> Self {
        ProgramError::Custom(e as u32)
    }
}
