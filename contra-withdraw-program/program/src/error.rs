use pinocchio::program_error::ProgramError;
use thiserror::Error;

/// Errors that may be returned by the Contra Withdraw Program.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum ContraWithdrawProgramError {
    /// (0) Invalid mint provided
    #[error("Invalid mint provided")]
    InvalidMint,
}

impl From<ContraWithdrawProgramError> for ProgramError {
    fn from(e: ContraWithdrawProgramError) -> Self {
        ProgramError::Custom(e as u32)
    }
}
