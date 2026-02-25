use contra_withdraw_program_client::instructions::WithdrawFundsBuilder;
use solana_sdk::{
    instruction::AccountMeta,
    signature::{Keypair, Signer},
};
use spl_associated_token_account::get_associated_token_address;
use spl_token::ID as TOKEN_PROGRAM_ID;

use crate::utils::{
    assert_program_error, set_mint, setup_test_balances, TestContext, ATA_PROGRAM_ID,
    INVALID_MINT_ERROR, MISSING_REQUIRED_SIGNATURE_ERROR,
};

const INITIAL_BALANCE: u64 = 1_000_000;
const WITHDRAW_AMOUNT: u64 = 500_000;

/// Wrong mint account should fail with InvalidMint.
#[test]
fn test_withdraw_funds_wrong_mint() {
    let mut context = TestContext::new();
    let user = Keypair::new();
    let mint = Keypair::new();
    let wrong_mint = Keypair::new();

    set_mint(&mut context, &mint.pubkey());
    setup_test_balances(&mut context, &user, &mint.pubkey(), INITIAL_BALANCE);

    let user_ata = get_associated_token_address(&user.pubkey(), &mint.pubkey());

    let instruction = WithdrawFundsBuilder::new()
        .user(user.pubkey())
        .mint(wrong_mint.pubkey()) // Wrong mint — no valid Mint data in SVM
        .token_account(user_ata)
        .token_program(TOKEN_PROGRAM_ID)
        .associated_token_program(ATA_PROGRAM_ID)
        .amount(WITHDRAW_AMOUNT)
        .instruction();

    let result = context.send_transaction_with_signers(instruction, &[&user]);

    assert_program_error(result, INVALID_MINT_ERROR);
}

/// Non-signer user should fail with MissingRequiredSignature.
#[test]
fn test_withdraw_funds_non_signer_user() {
    let mut context = TestContext::new();
    let user = Keypair::new();
    let mint = Keypair::new();

    set_mint(&mut context, &mint.pubkey());
    setup_test_balances(&mut context, &user, &mint.pubkey(), INITIAL_BALANCE);

    let user_ata = get_associated_token_address(&user.pubkey(), &mint.pubkey());

    // Build canonical instruction, then strip the signer flag from user account
    let mut instruction = WithdrawFundsBuilder::new()
        .user(user.pubkey())
        .mint(mint.pubkey())
        .token_account(user_ata)
        .token_program(TOKEN_PROGRAM_ID)
        .associated_token_program(ATA_PROGRAM_ID)
        .amount(WITHDRAW_AMOUNT)
        .instruction();

    instruction.accounts[0] = AccountMeta::new_readonly(user.pubkey(), false);

    let result = context.send_transaction_with_signers(instruction, &[]);

    assert_program_error(result, MISSING_REQUIRED_SIGNATURE_ERROR);
}
