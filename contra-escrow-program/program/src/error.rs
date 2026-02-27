use pinocchio::error::ProgramError;
use thiserror::Error;

/// Errors that may be returned by the Contra Escrow Program.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum ContraEscrowProgramError {
    /// (0) Invalid event authority provided
    #[error("Invalid event authority provided")]
    InvalidEventAuthority,

    /// (1) Invalid ATA provided
    #[error("Invalid ATA provided")]
    InvalidAta,

    /// (2) Invalid mint provided
    #[error("Invalid mint provided")]
    InvalidMint,

    /// (3) Instance ID invalid or does not respect rules
    #[error("Instance ID invalid or does not respect rules")]
    InvalidInstanceId,

    /// (4) Invalid instance provided
    #[error("Invalid instance provided")]
    InvalidInstance,

    /// (5) Invalid admin provided
    #[error("Invalid admin provided")]
    InvalidAdmin,

    /// (6) Permanent delegate extension not allowed
    #[error("Permanent delegate extension not allowed")]
    PermanentDelegateNotAllowed,

    /// (7) Pausable mint extension not allowed
    #[error("Pausable mint extension not allowed")]
    PausableMintNotAllowed,

    /// (8) Invalid operator PDA provided
    #[error("Invalid operator PDA provided")]
    InvalidOperatorPda,

    /// (9) Invalid token account provided
    #[error("Invalid token account provided")]
    InvalidTokenAccount,

    /// (10) Invalid escrow balance
    #[error("Invalid escrow balance")]
    InvalidEscrowBalance,

    /// (11) Invalid allowed mint
    #[error("Invalid allowed mint")]
    InvalidAllowedMint,

    /// (12) Invalid SMT proof provided
    #[error("Invalid SMT proof provided")]
    InvalidSmtProof,

    /// (13) Invalid transaction nonce for current tree index
    #[error("Invalid transaction nonce for current tree index")]
    InvalidTransactionNonceForCurrentTreeIndex,
}

impl From<ContraEscrowProgramError> for ProgramError {
    fn from(e: ContraEscrowProgramError) -> Self {
        ProgramError::Custom(e as u32)
    }
}
