use {
    solana_sdk::{signature::Keypair, signer::Signer},
    std::time::Duration,
};

use super::{
    test_context::PrivateChannelContext,
    utils::{MINT_DECIMALS, SEND_AND_CHECK_DURATION_SECONDS},
};
use crate::setup;

/// Test that non-admin users cannot send admin instructions
pub async fn run_non_admin_sending_admin_instruction_test(ctx: &PrivateChannelContext) {
    println!("\n=== Testing Non-Admin Sending Admin Instruction ===");

    let non_admin = Keypair::new();
    let fake_mint = Keypair::new();

    println!("Non-admin user: {}", non_admin.pubkey());
    println!(
        "Fake mint (for InitializeMint test): {}",
        fake_mint.pubkey()
    );

    // Try to initialize a mint as non-admin (should not land)
    // InitializeMint is the only admin instruction for SPL Token
    println!("Attempting to initialize mint as non-admin (should fail)...");
    let blockhash = ctx.get_blockhash().await.unwrap();
    let init_mint_tx = setup::create_mint_account_transaction(
        &non_admin,
        &fake_mint,
        &non_admin.pubkey(),
        MINT_DECIMALS,
        blockhash,
    );

    let result = ctx
        .send_and_check(
            &init_mint_tx,
            Duration::from_secs(SEND_AND_CHECK_DURATION_SECONDS),
        )
        .await
        .unwrap();
    assert!(
        result.is_none(),
        "Non-admin InitializeMint transaction {:?} should not have landed but it did!",
        result
    );

    println!("✓ Non-admin InitializeMint instruction correctly rejected");
}
