//! Integration test for `MintToBuilder` field-validation guards in
//! `indexer/src/operator/utils/instruction_util.rs`. Covers the five
//! `InvalidBuilder` error arms in `.instructions()` and `.instruction()`
//! that fire when a required field is left unset.
//!
//! Production callers use the typed fluent API which always sets every
//! field, but the defensive checks still exist — this test pins the
//! error private_channelct for each missing field.

use {
    private_channel_indexer::operator::utils::instruction_util::MintToBuilder, solana_sdk::pubkey::Pubkey,
};

fn pk(seed: u8) -> Pubkey {
    let mut b = [0u8; 32];
    b[0] = seed;
    Pubkey::new_from_array(b)
}

/// Helper: populate every required field on the builder so subsequent
/// tests can omit exactly one and assert that field's error arm fires.
/// `token_program` must be the real SPL Token program id — `mint_to()`
/// inside the SDK checks this before constructing the instruction.
fn fully_populated() -> MintToBuilder {
    let mut b = MintToBuilder::new();
    b.mint(pk(1))
        .recipient(pk(2))
        .recipient_ata(pk(3))
        .payer(pk(4))
        .mint_authority(pk(5))
        .token_program(spl_token::id())
        .amount(1_000);
    b
}

/// Sanity: the fully-populated builder compiles into a three-instruction
/// vector (create_ata + mint_to + no memo). Without this we can't trust
/// the per-field negative tests below.
#[test]
fn fully_populated_builder_produces_instructions() {
    let b = fully_populated();
    let ix = b.instructions().expect("full builder should succeed");
    assert!(
        ix.len() >= 2,
        "expected at least create_ata + mint_to, got {} instructions",
        ix.len()
    );
}

#[test]
fn missing_mint_errors_with_invalid_builder() {
    let mut b = MintToBuilder::new();
    b.recipient(pk(2))
        .recipient_ata(pk(3))
        .payer(pk(4))
        .mint_authority(pk(5))
        .token_program(spl_token::id())
        .amount(1_000);
    let err = b.instructions().expect_err("missing mint → Err");
    let msg = format!("{:?}", err);
    assert!(msg.contains("mint"), "error should mention 'mint': {msg}");
}

#[test]
fn missing_recipient_errors_with_invalid_builder() {
    let mut b = MintToBuilder::new();
    b.mint(pk(1))
        .recipient_ata(pk(3))
        .payer(pk(4))
        .mint_authority(pk(5))
        .token_program(spl_token::id())
        .amount(1_000);
    let err = b.instructions().expect_err("missing recipient → Err");
    let msg = format!("{:?}", err);
    assert!(
        msg.contains("recipient"),
        "error should mention 'recipient': {msg}"
    );
}

#[test]
fn missing_payer_errors_with_invalid_builder() {
    let mut b = MintToBuilder::new();
    b.mint(pk(1))
        .recipient(pk(2))
        .recipient_ata(pk(3))
        .mint_authority(pk(5))
        .token_program(spl_token::id())
        .amount(1_000);
    let err = b.instructions().expect_err("missing payer → Err");
    let msg = format!("{:?}", err);
    assert!(msg.contains("payer"), "error should mention 'payer': {msg}");
}

#[test]
fn missing_token_program_errors_with_invalid_builder() {
    let mut b = MintToBuilder::new();
    b.mint(pk(1))
        .recipient(pk(2))
        .recipient_ata(pk(3))
        .payer(pk(4))
        .mint_authority(pk(5))
        .amount(1_000);
    let err = b.instructions().expect_err("missing token_program → Err");
    let msg = format!("{:?}", err);
    assert!(
        msg.contains("token_program"),
        "error should mention 'token_program': {msg}"
    );
}

#[test]
fn missing_mint_authority_errors_in_inner_instruction_call() {
    let mut b = MintToBuilder::new();
    b.mint(pk(1))
        .recipient(pk(2))
        .recipient_ata(pk(3))
        .payer(pk(4))
        .token_program(spl_token::id())
        .amount(1_000);
    // `.instructions()` fans out to `.instruction()` after validating
    // the ATA-create args; the missing mint_authority surfaces as an
    // InvalidBuilder error from the inner `mint_to` call.
    let err = b.instructions().expect_err("missing mint_authority → Err");
    let msg = format!("{:?}", err);
    assert!(
        msg.contains("mint_authority"),
        "error should mention 'mint_authority': {msg}"
    );
}

#[test]
fn missing_amount_errors_in_inner_instruction_call() {
    let mut b = MintToBuilder::new();
    b.mint(pk(1))
        .recipient(pk(2))
        .recipient_ata(pk(3))
        .payer(pk(4))
        .mint_authority(pk(5))
        .token_program(spl_token::id());
    let err = b.instructions().expect_err("missing amount → Err");
    let msg = format!("{:?}", err);
    assert!(
        msg.contains("amount"),
        "error should mention 'amount': {msg}"
    );
}
