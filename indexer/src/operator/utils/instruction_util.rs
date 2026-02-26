use crate::error::ProgramError;
use crate::operator::{
    is_mint_not_initialized_error, ConfirmationResult, SignerUtil, DEFAULT_CU_MINT,
    DEFAULT_CU_RELEASE_FUNDS, MINT_IDEMPOTENCY_MEMO_PREFIX,
};
use contra_escrow_program_client::instructions::{ReleaseFundsBuilder, ResetSmtRootBuilder};
use solana_keychain::Signer;
use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::pubkey::Pubkey;
use spl_token::instruction::mint_to;
use std::fmt::Display;

/*
Mint initialization is going to be done outside of the operator. There's a command that will add to the allowed mints on Solana mainnet
and will also initialize that mint on Contra. This simplifies our operator's code and reduces the checks it needs to do if we'd want to
validate mint existence on Contra.
*/

pub fn mint_idempotency_memo(transaction_id: impl Display) -> String {
    format!("{MINT_IDEMPOTENCY_MEMO_PREFIX}{transaction_id}")
}

/// Retry policy for transaction submission
/// Controls whether failed transaction sends should be retried
#[derive(Clone, Debug, Copy)]
pub enum RetryPolicy {
    /// No retry - for non-idempotent operations where duplicate sends would cause issues
    None,
    /// Retry with exponential backoff - safe for idempotent operations
    Idempotent,
}

pub type ExtraErrorCheckFn = Box<
    dyn Fn(&solana_sdk::transaction::TransactionError) -> Option<ConfirmationResult>
        + Send
        + Sync
        + 'static,
>;

// Extra error check policy for transaction submission
pub enum ExtraErrorCheckPolicy {
    /// No extra error checks
    None,
    /// Extra error checks
    Extra(Vec<ExtraErrorCheckFn>),
}

/// Wrapper enum for different transaction builder types
/// Allows processor to send multiple builder types through a single channel to sender
#[derive(Clone, Debug)]
pub enum TransactionBuilder {
    /// Release funds transaction (Contra → L1) - requires SMT proof
    ReleaseFunds(Box<ReleaseFundsBuilderWithNonce>),
    /// Initialize mint transaction (L1 → Contra) - simple initialize_mint instruction
    InitializeMint(Box<InitializeMintBuilder>),
    /// Mint transaction (L1 → Contra) - simple SPL mint, no proof needed
    Mint(Box<MintToBuilderWithTxnId>),
    /// Reset SMT root transaction - rotates to new tree
    ResetSmtRoot(Box<ResetSmtRootBuilder>),
}

impl TransactionBuilder {
    pub fn instructions(&self) -> Result<Vec<Instruction>, crate::error::ProgramError> {
        match self {
            Self::ReleaseFunds(builder_with_nonce) => {
                Ok(vec![builder_with_nonce.builder.instruction()])
            }
            Self::InitializeMint(builder) => Ok(vec![builder.instruction()?]),
            Self::Mint(builder_with_txn_id) => builder_with_txn_id.builder.instructions(),
            Self::ResetSmtRoot(builder) => Ok(vec![builder.instruction()]),
        }
    }

    pub fn compute_unit_price(&self) -> Option<u64> {
        match self {
            Self::ReleaseFunds(_) => Some(1),
            Self::InitializeMint(_) => None,
            Self::Mint(_) => None,
            Self::ResetSmtRoot(_) => Some(1),
        }
    }

    /// Get optional compute budget for this transaction type (in compute units)
    /// Returns None if default compute budget (200k CU) is sufficient
    pub fn compute_budget(&self) -> Option<u32> {
        match self {
            Self::ReleaseFunds(_) => DEFAULT_CU_RELEASE_FUNDS,
            Self::InitializeMint(_) => DEFAULT_CU_MINT,
            Self::Mint(_) => DEFAULT_CU_MINT,
            Self::ResetSmtRoot(_) => DEFAULT_CU_MINT,
        }
    }

    pub fn signers(&self) -> Vec<&'static Signer> {
        match self {
            Self::ReleaseFunds(_) => {
                vec![SignerUtil::admin_signer(), SignerUtil::operator_signer()]
            }
            Self::InitializeMint(_) => vec![SignerUtil::admin_signer()],
            Self::Mint(_) => vec![SignerUtil::admin_signer()],
            Self::ResetSmtRoot(_) => {
                vec![SignerUtil::admin_signer(), SignerUtil::operator_signer()]
            }
        }
    }

    /// Get the database transaction ID for storage/logging operations
    /// Returns the DB id for all transaction types with a DB record
    pub fn transaction_id(&self) -> Option<i64> {
        match self {
            TransactionBuilder::ReleaseFunds(builder) => Some(builder.transaction_id),
            TransactionBuilder::InitializeMint(_) => None,
            TransactionBuilder::Mint(builder) => Some(builder.txn_id),
            TransactionBuilder::ResetSmtRoot(_) => None,
        }
    }

    /// Get the withdrawal nonce for SMT/nonce-based operations
    /// Returns nonce only for ReleaseFunds (withdrawal) transactions
    pub fn withdrawal_nonce(&self) -> Option<u64> {
        match self {
            TransactionBuilder::ReleaseFunds(builder) => Some(builder.nonce),
            TransactionBuilder::InitializeMint(_) => None,
            TransactionBuilder::Mint(_) => None,
            TransactionBuilder::ResetSmtRoot(_) => None,
        }
    }

    /// Get retry policy for this transaction type
    ///
    /// # Retry Policies by Transaction Type
    /// - **InitializeMint**: Idempotent retry - Safe to retry if mint already initialized.
    /// - **Mint**: No sender-level retry - retries happen only after memo-based idempotency
    ///   verification to prevent duplicate issuance.
    /// - **ReleaseFunds**: Idempotent retry - Uses transaction nonce to prevent duplicates.
    ///   Safe to retry on transient network failures.
    /// - **ResetSmtRoot**: Idempotent retry - tree_index increments ensure idempotency.
    ///   Safe to retry on transient network failures.
    pub fn retry_policy(&self) -> RetryPolicy {
        match self {
            Self::ReleaseFunds(_) => RetryPolicy::Idempotent,
            Self::InitializeMint(_) => RetryPolicy::Idempotent,
            Self::Mint(_) => RetryPolicy::None,
            Self::ResetSmtRoot(_) => RetryPolicy::Idempotent,
        }
    }

    /// Get extra error checks for this transaction type
    pub fn extra_error_checks_policy(&self) -> ExtraErrorCheckPolicy {
        match self {
            Self::ReleaseFunds(_) => ExtraErrorCheckPolicy::None,
            Self::InitializeMint(_) => ExtraErrorCheckPolicy::None,
            Self::Mint(_) => {
                ExtraErrorCheckPolicy::Extra(vec![Box::new(is_mint_not_initialized_error)])
            }
            Self::ResetSmtRoot(_) => ExtraErrorCheckPolicy::None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ReleaseFundsBuilderWithNonce {
    pub builder: ReleaseFundsBuilder,
    // Nonce is the transaction id but for withdrawals only
    // So that deposits don't count for the SMT tree rotation
    pub nonce: u64,
    pub transaction_id: i64,
}

/// Builder for simple SPL token mint instructions (deposit flow)
/// Creates ATA idempotently, then mints tokens
#[derive(Clone, Debug, Default)]
pub struct MintToBuilder {
    mint: Option<Pubkey>,
    recipient: Option<Pubkey>,
    recipient_ata: Option<Pubkey>,
    payer: Option<Pubkey>,
    mint_authority: Option<Pubkey>,
    token_program: Option<Pubkey>,
    amount: Option<u64>,
    idempotency_memo: Option<String>,
}

impl MintToBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mint(&mut self, mint: Pubkey) -> &mut Self {
        self.mint = Some(mint);
        self
    }

    pub fn recipient(&mut self, recipient: Pubkey) -> &mut Self {
        self.recipient = Some(recipient);
        self
    }

    pub fn recipient_ata(&mut self, recipient_ata: Pubkey) -> &mut Self {
        self.recipient_ata = Some(recipient_ata);
        self
    }

    pub fn payer(&mut self, payer: Pubkey) -> &mut Self {
        self.payer = Some(payer);
        self
    }

    pub fn mint_authority(&mut self, mint_authority: Pubkey) -> &mut Self {
        self.mint_authority = Some(mint_authority);
        self
    }

    pub fn token_program(&mut self, token_program: Pubkey) -> &mut Self {
        self.token_program = Some(token_program);
        self
    }

    pub fn amount(&mut self, amount: u64) -> &mut Self {
        self.amount = Some(amount);
        self
    }

    pub fn idempotency_memo(&mut self, memo: String) -> &mut Self {
        self.idempotency_memo = Some(memo);
        self
    }

    pub fn get_mint(&self) -> Option<Pubkey> {
        self.mint
    }

    pub fn get_token_program(&self) -> Option<Pubkey> {
        self.token_program
    }

    pub fn get_payer(&self) -> Option<Pubkey> {
        self.payer
    }

    pub fn get_mint_authority(&self) -> Option<Pubkey> {
        self.mint_authority
    }

    pub fn get_amount(&self) -> Option<u64> {
        self.amount
    }

    pub fn get_recipient_ata(&self) -> Option<Pubkey> {
        self.recipient_ata
    }

    /// Returns instructions: [create_ata_idempotent, optional_memo, mint_to]
    pub fn instructions(&self) -> Result<Vec<Instruction>, crate::error::ProgramError> {
        let mint = self.mint.ok_or_else(|| ProgramError::InvalidBuilder {
            reason: "mint not set".to_string(),
        })?;
        let recipient = self.recipient.ok_or_else(|| ProgramError::InvalidBuilder {
            reason: "recipient not set".to_string(),
        })?;
        let payer = self.payer.ok_or_else(|| ProgramError::InvalidBuilder {
            reason: "payer not set".to_string(),
        })?;
        let token_program = self
            .token_program
            .ok_or_else(|| ProgramError::InvalidBuilder {
                reason: "token_program not set".to_string(),
            })?;

        let mut instructions = vec![
            spl_associated_token_account::instruction::create_associated_token_account_idempotent(
                &payer,
                &recipient,
                &mint,
                &token_program,
            ),
        ];

        if let Some(memo) = self.idempotency_memo.as_deref() {
            instructions.push(Instruction {
                program_id: spl_memo::id(),
                accounts: vec![AccountMeta::new_readonly(payer, true)],
                data: memo.as_bytes().to_vec(),
            });
        }

        instructions.push(self.instruction()?);

        Ok(instructions)
    }

    pub fn instruction(&self) -> Result<Instruction, crate::error::ProgramError> {
        mint_to(
            &self
                .token_program
                .ok_or_else(|| ProgramError::InvalidBuilder {
                    reason: "token_program not set".to_string(),
                })?,
            &self.mint.ok_or_else(|| ProgramError::InvalidBuilder {
                reason: "mint not set".to_string(),
            })?,
            &self
                .recipient_ata
                .ok_or_else(|| ProgramError::InvalidBuilder {
                    reason: "recipient_ata not set".to_string(),
                })?,
            &self
                .mint_authority
                .ok_or_else(|| ProgramError::InvalidBuilder {
                    reason: "mint_authority not set".to_string(),
                })?,
            &[],
            self.amount.ok_or_else(|| ProgramError::InvalidBuilder {
                reason: "amount not set".to_string(),
            })?,
        )
        .map_err(|e| ProgramError::InvalidBuilder {
            reason: format!("failed to build mint_to instruction: {}", e),
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct MintToBuilderWithTxnId {
    pub builder: MintToBuilder,
    pub txn_id: i64,
}

/// Builder for initialize_mint instruction (sent before first mint)
#[derive(Clone, Debug)]
pub struct InitializeMintBuilder {
    pub mint: Pubkey,
    pub decimals: u8,
    pub mint_authority: Pubkey,
    pub token_program: Pubkey,
    pub payer: Pubkey,
}

impl InitializeMintBuilder {
    pub fn new(
        mint: Pubkey,
        decimals: u8,
        mint_authority: Pubkey,
        token_program: Pubkey,
        payer: Pubkey,
    ) -> Self {
        Self {
            mint,
            decimals,
            mint_authority,
            token_program,
            payer,
        }
    }

    pub fn instruction(&self) -> Result<Instruction, crate::error::ProgramError> {
        spl_token::instruction::initialize_mint(
            &self.token_program,
            &self.mint,
            &self.mint_authority,
            Some(&self.mint_authority),
            self.decimals,
        )
        .map_err(|e| ProgramError::InvalidBuilder {
            reason: format!("failed to build initialize_mint: {}", e),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::mint_idempotency_memo;

    #[test]
    fn mint_idempotency_memo_supports_i64() {
        assert_eq!(
            mint_idempotency_memo(42_i64),
            "contra:mint-idempotency:42".to_string()
        );
    }

    #[test]
    fn mint_idempotency_memo_supports_u64() {
        assert_eq!(
            mint_idempotency_memo(42_u64),
            "contra:mint-idempotency:42".to_string()
        );
    }
}
