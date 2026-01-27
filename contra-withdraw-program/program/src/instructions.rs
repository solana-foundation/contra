extern crate alloc;

use pinocchio::pubkey::Pubkey;
use shank::ShankInstruction;

/// Instructions for the Solana Contra Withdraw Program.
#[repr(C, u8)]
#[derive(Clone, Debug, PartialEq, ShankInstruction)]
pub enum ContraWithdrawProgramInstruction {
    /// Withdraw funds from a token account to itself or to a destination (if provided)
    #[account(
        0,
        signer,
        name = "user",
        description = "User initiating the withdrawal"
    )]
    #[account(1, writable, name = "mint", description = "Token mint")]
    #[account(
        2,
        writable,
        name = "token_account",
        description = "Source token account"
    )]
    #[account(3, name = "token_program", description = "Token program")]
    #[account(
        4,
        name = "associated_token_program",
        description = "Associated token program"
    )]
    WithdrawFunds {
        /// Amount of tokens to withdraw
        amount: u64,
        /// Destination public key
        destination: Option<Pubkey>,
    } = 0,
}
