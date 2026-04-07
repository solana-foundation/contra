use std::collections::HashMap;

use contra_escrow_program_client::instructions::ReleaseFundsBuilder;
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};
use spl_associated_token_account::get_associated_token_address_with_program_id;
use spl_token::ID as TOKEN_PROGRAM_ID;
use tests_contra_escrow_program::{
    pda_utils::{find_allowed_mint_pda, find_event_authority_pda},
    smt_utils::ProcessorSMT,
    state_utils::{
        assert_get_or_add_operator, assert_get_or_allow_mint, assert_get_or_create_instance,
    },
    utils::{set_mint, set_token_balance, ATA_PROGRAM_ID, CONTRA_ESCROW_PROGRAM_ID, TestContext},
};

/// Data retained from a successful release so `DoubleSpend` can replay it.
#[derive(Clone, Debug)]
pub struct SuccessfulRelease {
    pub amount: u64,
    pub new_withdrawal_root: [u8; 32],
    pub sibling_proofs: [u8; 512],
}

/// Everything the harness needs to drive one fuzz case.
pub struct FuzzContext {
    pub test_context: TestContext,
    pub operator: Keypair,
    pub user: Keypair,
    pub mint: Pubkey,
    pub instance_pda: Pubkey,
    pub operator_pda: Pubkey,
    pub user_ata: Pubkey,
    /// Local mirror of the on-chain SMT; kept in sync after every successful release.
    pub smt: ProcessorSMT,
    /// Maps `nonce -> release data` for every release that succeeded on-chain.
    pub successful_releases: HashMap<u64, SuccessfulRelease>,
}

/// Keep amounts small enough to avoid accidental overflow while still
/// exercising non-trivial values.
pub fn clamp_amount(raw: u64) -> u64 {
    (raw % 1_000_000).max(1)
}

/// Keep nonces within the first 1 000 slots of the tree so the harness stays
/// within tree-index 0 (nonces 0..65 535).
pub fn clamp_nonce(raw: u64) -> u64 {
    raw % 1_000
}

/// Build a fresh escrow environment: one instance, one allowed mint, one
/// operator, and one user pre-funded with 10 trillion token units.
pub fn setup_fuzz_context() -> FuzzContext {
    let mut context = TestContext::new();
    let admin = Keypair::new();
    let operator = Keypair::new();
    let user = Keypair::new();
    let mint_keypair = Keypair::new();
    let mint = mint_keypair.pubkey();

    let instance_seed = Keypair::new();
    let (instance_pda, _) =
        assert_get_or_create_instance(&mut context, &admin, &instance_seed, true, false)
            .expect("create_instance failed");

    set_mint(&mut context, &mint);

    assert_get_or_allow_mint(&mut context, &admin, &instance_pda, &mint, true, false)
        .expect("allow_mint failed");

    let (operator_pda, _) = assert_get_or_add_operator(
        &mut context,
        &admin,
        &instance_pda,
        &operator.pubkey(),
        true,
        false,
    )
    .expect("add_operator failed");

    let user_ata =
        get_associated_token_address_with_program_id(&user.pubkey(), &mint, &TOKEN_PROGRAM_ID);

    set_token_balance(
        &mut context,
        &user_ata,
        &mint,
        &user.pubkey(),
        10_000_000_000_000,
    );

    FuzzContext {
        test_context: context,
        operator,
        user,
        mint,
        instance_pda,
        operator_pda,
        user_ata,
        smt: ProcessorSMT::new(),
        successful_releases: HashMap::new(),
    }
}

/// Assemble a `ReleaseFunds` instruction from its individual components.
pub fn build_release_ix(
    context: &TestContext,
    operator: &Keypair,
    user: &Keypair,
    mint: Pubkey,
    instance_pda: Pubkey,
    operator_pda: Pubkey,
    user_ata: Pubkey,
    instance_ata: Pubkey,
    amount: u64,
    nonce: u64,
    new_withdrawal_root: [u8; 32],
    sibling_proofs: [u8; 512],
) -> solana_sdk::instruction::Instruction {
    let (allowed_mint_pda, _) = find_allowed_mint_pda(&instance_pda, &mint);
    let (event_authority_pda, _) = find_event_authority_pda();

    ReleaseFundsBuilder::new()
        .payer(context.payer.pubkey())
        .operator(operator.pubkey())
        .instance(instance_pda)
        .operator_pda(operator_pda)
        .mint(mint)
        .allowed_mint(allowed_mint_pda)
        .user_ata(user_ata)
        .instance_ata(instance_ata)
        .token_program(TOKEN_PROGRAM_ID)
        .associated_token_program(ATA_PROGRAM_ID)
        .event_authority(event_authority_pda)
        .contra_escrow_program(CONTRA_ESCROW_PROGRAM_ID)
        .amount(amount)
        .user(user.pubkey())
        .new_withdrawal_root(new_withdrawal_root)
        .transaction_nonce(nonce)
        .sibling_proofs(sibling_proofs)
        .instruction()
}
