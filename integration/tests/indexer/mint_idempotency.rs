#[path = "helpers/mod.rs"]
mod helpers;

use contra_indexer::operator::{
    find_existing_mint_signature, mint_idempotency_memo, MintToBuilder, MintToBuilderWithTxnId,
    RetryConfig, RpcClientWithRetry,
};
use helpers::{generate_mint, send_and_confirm_instructions, setup_wallets};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    instruction::{AccountMeta, Instruction},
    signature::{Keypair, Signer},
};
use spl_associated_token_account::get_associated_token_address_with_program_id;
use std::sync::Arc;
use test_utils::validator_helper::start_test_validator;

#[tokio::test(flavor = "multi_thread")]
async fn find_existing_mint_signature_detects_confirmed_mint() {
    let (validator, faucet_keypair, _geyser_port) = start_test_validator().await;
    let rpc_url = validator.rpc_url();
    let client = RpcClient::new_with_commitment(rpc_url.clone(), CommitmentConfig::confirmed());

    let payer = Keypair::new();
    let authority = Keypair::new();
    let mint_kp = Keypair::new();
    setup_wallets(&client, &faucet_keypair, &[&payer, &authority])
        .await
        .unwrap();

    generate_mint(&client, &payer, &authority, &mint_kp)
        .await
        .unwrap();

    let recipient = Keypair::new();
    let recipient_ata = get_associated_token_address_with_program_id(
        &recipient.pubkey(),
        &mint_kp.pubkey(),
        &spl_token::id(),
    );

    let txn_id: i64 = 42;
    let amount: u64 = 1000;
    let memo = mint_idempotency_memo(txn_id);

    let create_ata_ix =
        spl_associated_token_account::instruction::create_associated_token_account_idempotent(
            &payer.pubkey(),
            &recipient.pubkey(),
            &mint_kp.pubkey(),
            &spl_token::id(),
        );
    let memo_ix = Instruction {
        program_id: spl_memo::id(),
        accounts: vec![AccountMeta::new_readonly(payer.pubkey(), true)],
        data: memo.as_bytes().to_vec(),
    };
    let mint_to_ix = spl_token::instruction::mint_to(
        &spl_token::id(),
        &mint_kp.pubkey(),
        &recipient_ata,
        &authority.pubkey(),
        &[],
        amount,
    )
    .unwrap();

    let sig = send_and_confirm_instructions(
        &client,
        &[create_ata_ix, memo_ix, mint_to_ix],
        &payer,
        &[&payer, &authority],
        "Mint with idempotency memo",
    )
    .await
    .unwrap();

    let rpc_client = Arc::new(RpcClientWithRetry::with_retry_config(
        rpc_url.clone(),
        RetryConfig::default(),
        CommitmentConfig::confirmed(),
    ));

    // Matching builder should find the signature
    let mut builder = MintToBuilder::new();
    builder
        .mint(mint_kp.pubkey())
        .recipient_ata(recipient_ata)
        .mint_authority(authority.pubkey())
        .token_program(spl_token::id())
        .amount(amount);
    let builder_with_id = MintToBuilderWithTxnId { builder, txn_id };

    let result = find_existing_mint_signature(&rpc_client, &builder_with_id)
        .await
        .unwrap();
    assert_eq!(result, Some(sig));

    // Different txn_id (different memo) should return None
    let mut builder2 = MintToBuilder::new();
    builder2
        .mint(mint_kp.pubkey())
        .recipient_ata(recipient_ata)
        .mint_authority(authority.pubkey())
        .token_program(spl_token::id())
        .amount(amount);
    let builder_with_wrong_id = MintToBuilderWithTxnId {
        builder: builder2,
        txn_id: 999,
    };

    let result2 = find_existing_mint_signature(&rpc_client, &builder_with_wrong_id)
        .await
        .unwrap();
    assert_eq!(result2, None);

    // Wrong amount should return None
    let mut builder3 = MintToBuilder::new();
    builder3
        .mint(mint_kp.pubkey())
        .recipient_ata(recipient_ata)
        .mint_authority(authority.pubkey())
        .token_program(spl_token::id())
        .amount(9999);
    let builder_wrong_amount = MintToBuilderWithTxnId {
        builder: builder3,
        txn_id,
    };

    let result3 = find_existing_mint_signature(&rpc_client, &builder_wrong_amount)
        .await
        .unwrap();
    assert_eq!(result3, None);
}
