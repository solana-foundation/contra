use {solana_sdk::signature::Keypair, std::time::Duration};

use super::{test_context::PrivateChannelContext, utils::SEND_AND_CHECK_DURATION_SECONDS};
use crate::setup;

/// Test that empty transactions are rejected
pub async fn run_empty_transaction_test(ctx: &PrivateChannelContext) {
    println!("\n=== Testing Empty Transactions ===");

    // Test 1: Admin sending empty transaction
    println!("Test 1: Admin sending empty transaction...");
    let blockhash = ctx.get_blockhash().await.unwrap();
    let empty_tx = setup::empty_transaction(&ctx.operator_key, blockhash);
    let result = ctx
        .send_and_check(
            &empty_tx,
            Duration::from_secs(SEND_AND_CHECK_DURATION_SECONDS),
        )
        .await
        .unwrap();
    assert!(
        result.is_none(),
        "Admin empty transaction {:?} should not have landed but it did!",
        result
    );
    println!("✓ Admin empty transaction correctly rejected");

    // Test 2: Non-admin sending empty transaction
    println!("\nTest 2: Non-admin sending empty transaction...");
    let non_admin = Keypair::new();
    let blockhash = ctx.get_blockhash().await.unwrap();
    let empty_tx = setup::empty_transaction(&non_admin, blockhash);
    let result = ctx
        .send_and_check(
            &empty_tx,
            Duration::from_secs(SEND_AND_CHECK_DURATION_SECONDS),
        )
        .await
        .unwrap();
    assert!(
        result.is_none(),
        "Non-admin empty transaction {:?} should not have landed but it did!",
        result
    );
    println!("✓ Non-admin empty transaction correctly rejected");
}
