extern crate alloc;

use pinocchio::Address as Pubkey;
use shank::ShankInstruction;

/// Instructions for the Solana Contra Escrow Program. This
/// is currently not used in the program business logic, but
/// we include it for IDL generation.
#[allow(clippy::large_enum_variant)]
#[repr(C, u8)]
#[derive(Clone, Debug, PartialEq, ShankInstruction)]
pub enum ContraEscrowProgramInstruction {
    /// Create a new escrow instance with the specified admin.
    #[account(
        0,
        signer,
        writable,
        name = "payer",
        description = "Transaction fee payer"
    )]
    #[account(1, signer, name = "admin", description = "Admin of Instance")]
    #[account(
        2,
        signer,
        name = "instance_seed",
        description = "Instance seed signer for PDA derivation"
    )]
    #[account(
        3,
        writable,
        name = "instance",
        description = "Instance PDA to be created"
    )]
    #[account(4, name = "system_program", description = "System program")]
    #[account(
        5,
        name = "event_authority",
        description = "Event authority PDA for emitting events"
    )]
    #[account(
        6,
        name = "contra_escrow_program",
        description = "Current program for CPI"
    )]
    CreateInstance {
        /// Bump for the instance PDA
        bump: u8,
    } = 0,

    /// Allow new token mints for the instance (admin-only).
    #[account(
        0,
        signer,
        writable,
        name = "payer",
        description = "Transaction fee payer"
    )]
    #[account(1, signer, name = "admin", description = "Admin of Instance")]
    #[account(
        2,
        name = "instance",
        description = "Instance PDA to validate admin authority"
    )]
    #[account(3, name = "mint", description = "Token mint to be allowed")]
    #[account(
        4,
        writable,
        name = "allowed_mint",
        description = "PDA of the Allowed Mint"
    )]
    #[account(
        5,
        writable,
        name = "instance_ata",
        description = "Instance Escrow account for specified mint"
    )]
    #[account(6, name = "system_program", description = "System program")]
    #[account(7, name = "token_program", description = "Token program")]
    #[account(
        8,
        name = "associated_token_program",
        description = "Associated Token program"
    )]
    #[account(
        9,
        name = "event_authority",
        description = "Event authority PDA for emitting events"
    )]
    #[account(
        10,
        name = "contra_escrow_program",
        description = "Current program for CPI"
    )]
    AllowMint {
        /// Bump for the allowed mint PDA
        bump: u8,
    } = 1,

    /// Block previously allowed mints for the instance (admin-only).
    #[account(
        0,
        signer,
        writable,
        name = "payer",
        description = "Transaction fee payer"
    )]
    #[account(1, signer, name = "admin", description = "Admin of Instance")]
    #[account(
        2,
        name = "instance",
        description = "Instance PDA to validate admin authority"
    )]
    #[account(3, name = "mint", description = "Token mint to be blocked")]
    #[account(
        4,
        writable,
        name = "allowed_mint",
        description = "Existing Allowed Mint PDA"
    )]
    #[account(
        5,
        name = "system_program",
        description = "System program for account creation"
    )]
    #[account(
        6,
        name = "event_authority",
        description = "Event authority PDA for emitting events"
    )]
    #[account(
        7,
        name = "contra_escrow_program",
        description = "Current program for CPI"
    )]
    BlockMint {} = 2,

    /// Add an operator to the instance (admin-only).
    #[account(
        0,
        signer,
        writable,
        name = "payer",
        description = "Transaction fee payer"
    )]
    #[account(1, signer, name = "admin", description = "Admin of Instance")]
    #[account(
        2,
        name = "instance",
        description = "Instance PDA to validate admin authority"
    )]
    #[account(3, name = "operator", description = "Operator public key to be added")]
    #[account(
        4,
        writable,
        name = "operator_pda",
        description = "Operator PDA to be created"
    )]
    #[account(5, name = "system_program", description = "System program")]
    #[account(
        6,
        name = "event_authority",
        description = "Event authority PDA for emitting events"
    )]
    #[account(
        7,
        name = "contra_escrow_program",
        description = "Current program for CPI"
    )]
    AddOperator {
        /// Bump for the operator PDA
        bump: u8,
    } = 3,

    /// Remove an operator from the instance (admin-only).
    #[account(
        0,
        signer,
        writable,
        name = "payer",
        description = "Transaction fee payer"
    )]
    #[account(1, signer, name = "admin", description = "Admin of Instance")]
    #[account(
        2,
        name = "instance",
        description = "Instance PDA to validate admin authority"
    )]
    #[account(
        3,
        name = "operator",
        description = "Operator public key to be removed"
    )]
    #[account(
        4,
        writable,
        name = "operator_pda",
        description = "Existing Operator PDA"
    )]
    #[account(5, name = "system_program", description = "System program")]
    #[account(
        6,
        name = "event_authority",
        description = "Event authority PDA for emitting events"
    )]
    #[account(
        7,
        name = "contra_escrow_program",
        description = "Current program for CPI"
    )]
    RemoveOperator {} = 4,

    /// Set a new admin for the instance (current admin only).
    #[account(
        0,
        signer,
        writable,
        name = "payer",
        description = "Transaction fee payer"
    )]
    #[account(
        1,
        signer,
        name = "current_admin",
        description = "Current admin of Instance"
    )]
    #[account(
        2,
        writable,
        name = "instance",
        description = "Instance PDA to update admin"
    )]
    #[account(3, signer, name = "new_admin", description = "New admin public key")]
    #[account(
        4,
        name = "event_authority",
        description = "Event authority PDA for emitting events"
    )]
    #[account(
        5,
        name = "contra_escrow_program",
        description = "Current program for CPI"
    )]
    SetNewAdmin {} = 5,

    /// Deposit tokens from user ATA to instance escrow ATA (permissionless).
    #[account(
        0,
        signer,
        writable,
        name = "payer",
        description = "Transaction fee payer"
    )]
    #[account(1, signer, name = "user", description = "User depositing tokens")]
    #[account(2, name = "instance", description = "Instance PDA to validate")]
    #[account(3, name = "mint", description = "Token mint being deposited")]
    #[account(
        4,
        name = "allowed_mint",
        description = "AllowedMint PDA to validate mint is allowed"
    )]
    #[account(
        5,
        writable,
        name = "user_ata",
        description = "User's Associated Token Account for this mint"
    )]
    #[account(
        6,
        writable,
        name = "instance_ata",
        description = "Instance's Associated Token Account (escrow) for this mint"
    )]
    #[account(7, name = "system_program", description = "System program")]
    #[account(8, name = "token_program", description = "Token program for the mint")]
    #[account(
        9,
        name = "associated_token_program",
        description = "Associated Token program"
    )]
    #[account(
        10,
        name = "event_authority",
        description = "Event authority PDA for emitting events"
    )]
    #[account(
        11,
        name = "contra_escrow_program",
        description = "Current program for CPI"
    )]
    Deposit {
        /// Amount of tokens to deposit
        amount: u64,
        /// Optional recipient for Contra tracking, is the wallet address, not the ATA (if None, defaults to user)
        recipient: Option<Pubkey>,
    } = 6,

    /// Release funds from escrow to user (operator-only).
    #[account(
        0,
        signer,
        writable,
        name = "payer",
        description = "Transaction fee payer"
    )]
    #[account(
        1,
        signer,
        name = "operator",
        description = "Operator releasing the funds"
    )]
    #[account(
        2,
        writable,
        name = "instance",
        description = "Instance PDA to validate and update"
    )]
    #[account(
        3,
        name = "operator_pda",
        description = "Operator PDA to validate operator permissions"
    )]
    #[account(4, name = "mint", description = "Token mint being released")]
    #[account(
        5,
        name = "allowed_mint",
        description = "AllowedMint PDA to validate mint is allowed"
    )]
    #[account(
        6,
        writable,
        name = "user_ata",
        description = "User's Associated Token Account for this mint"
    )]
    #[account(
        7,
        writable,
        name = "instance_ata",
        description = "Instance's Associated Token Account (escrow) for this mint"
    )]
    #[account(8, name = "token_program", description = "Token program for the mint")]
    #[account(
        9,
        name = "associated_token_program",
        description = "Associated Token program"
    )]
    #[account(
        10,
        name = "event_authority",
        description = "Event authority PDA for emitting events"
    )]
    #[account(
        11,
        name = "contra_escrow_program",
        description = "Current program for CPI"
    )]
    ReleaseFunds {
        /// Amount of tokens to release
        amount: u64,
        /// User receiving the funds (wallet address, not the ATA)
        user: Pubkey,
        /// New withdrawal transactions root
        new_withdrawal_root: [u8; 32],
        /// Transaction nonce
        transaction_nonce: u64,
        /// Sibling proofs (flattened as 512 bytes: 16 proofs × 32 bytes each, because of Shank limitation)
        sibling_proofs: [u8; 512],
    } = 7,

    /// Reset the SMT root for the instance (operator-only).
    #[account(
        0,
        signer,
        writable,
        name = "payer",
        description = "Transaction fee payer"
    )]
    #[account(
        1,
        signer,
        name = "operator",
        description = "Operator resetting the SMT root"
    )]
    #[account(2, writable, name = "instance", description = "Instance PDA to reset")]
    #[account(
        3,
        name = "operator_pda",
        description = "Operator PDA to validate operator permissions"
    )]
    #[account(
        4,
        name = "event_authority",
        description = "Event authority PDA for emitting events"
    )]
    #[account(
        5,
        name = "contra_escrow_program",
        description = "Current program for CPI"
    )]
    ResetSmtRoot {} = 8,

    /// Invoked via CPI from another program to log event via instruction data.
    #[account(
        0,
        signer,
        name = "event_authority",
        description = "Event authority PDA for emitting events"
    )]
    EmitEvent {} = 228,
}
