extern crate alloc;

use codama::CodamaInstructions;
use pinocchio::Address as Pubkey;

/// Instructions for the Solana Contra Withdraw Program.
#[repr(C, u8)]
#[derive(Clone, Debug, PartialEq, CodamaInstructions)]
pub enum ContraWithdrawProgramInstruction {
    /// Withdraw funds from a token account to itself or to a destination (if provided)
    #[codama(account(name = "user", docs = "User initiating the withdrawal", signer))]
    #[codama(account(name = "mint", docs = "Token mint", writable))]
    #[codama(account(name = "token_account", docs = "Source token account", writable))]
    #[codama(account(name = "token_program", docs = "Token program"))]
    #[codama(account(name = "associated_token_program", docs = "Associated token program"))]
    WithdrawFunds {
        /// Amount of tokens to withdraw
        amount: u64,
        /// Destination public key
        destination: Option<Pubkey>,
    } = 0,
}
