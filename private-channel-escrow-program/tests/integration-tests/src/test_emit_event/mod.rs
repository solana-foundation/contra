use crate::utils::{
    assert_program_error, TestContext, INVALID_EVENT_AUTHORITY_ERROR,
    PRIVATE_CHANNEL_ESCROW_PROGRAM_ID,
};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    signature::Keypair,
    signer::Signer,
};

#[test]
fn test_emit_event_wrong_event_authority() {
    let mut context = TestContext::new();
    let wrong_authority = Keypair::new();

    context
        .airdrop_if_required(&wrong_authority.pubkey(), 1_000_000_000)
        .unwrap();

    // Discriminator 228 routes to process_emit_event.
    // Passing any address other than the canonical event_authority PDA must fail.
    let accounts = vec![AccountMeta::new_readonly(wrong_authority.pubkey(), true)];

    let instruction = Instruction {
        program_id: PRIVATE_CHANNEL_ESCROW_PROGRAM_ID,
        accounts,
        data: vec![228],
    };

    let result = context.send_transaction_with_signers(instruction, &[&wrong_authority]);

    assert_program_error(result, INVALID_EVENT_AUTHORITY_ERROR);
}

#[test]
fn test_emit_event_no_accounts() {
    let mut context = TestContext::new();

    let instruction = Instruction {
        program_id: PRIVATE_CHANNEL_ESCROW_PROGRAM_ID,
        accounts: vec![],
        data: vec![228],
    };

    let result = context.send_transaction(instruction);

    assert_program_error(result, crate::utils::NOT_ENOUGH_ACCOUNT_KEYS_ERROR);
}
