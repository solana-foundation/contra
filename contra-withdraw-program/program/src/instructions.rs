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
    #[account(
        5,
        name = "event_authority",
        description = "Event authority PDA for emitting events"
    )]
    #[account(
        6,
        name = "contra_withdraw_program",
        description = "Current program for CPI"
    )]
    WithdrawFunds {
        /// Amount of tokens to withdraw
        amount: u64,
        /// Destination public key
        destination: Option<Pubkey>,
    } = 0,

    /// Invoked via CPI from this program to log event via instruction data.
    #[account(
        0,
        signer,
        name = "event_authority",
        description = "Event authority PDA for emitting events"
    )]
    EmitEvent {} = 228,
}
