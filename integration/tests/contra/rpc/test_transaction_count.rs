use {
    super::test_context::ContraContext,
    solana_sdk::{signature::Keypair, signer::Signer},
    solana_system_interface::instruction as system_instruction,
};

pub async fn run_transaction_count_test(ctx: &ContraContext) {
    println!("\n=== Transaction Count Test ===");

    // Get initial transaction count
    let initial_count = ctx.get_transaction_count().await.unwrap();
    println!("Initial transaction count: {}", initial_count);

    // Create a simple transfer transaction to increment the count
    let from_keypair = Keypair::new();
    let to_pubkey = Keypair::new().pubkey();

    let blockhash = ctx.get_blockhash().await.unwrap();
    let transfer_ix = system_instruction::transfer(&from_keypair.pubkey(), &to_pubkey, 1_000);

    let transaction = solana_sdk::transaction::Transaction::new_signed_with_payer(
        &[transfer_ix],
        Some(&from_keypair.pubkey()),
        &[&from_keypair],
        blockhash,
    );

    // Send the transaction
    let sig = ctx.send_transaction(&transaction).await.unwrap();
    println!("Sent transaction: {}", sig);

    // Wait for confirmation
    ctx.check_transaction_exists(sig).await;
    println!("Transaction confirmed: {}", sig);

    // Check that transaction count has increased
    let new_count = ctx.get_transaction_count().await.unwrap();
    println!("New transaction count: {}", new_count);

    assert_eq!(
        new_count,
        initial_count + 1,
        "Transaction count should have increased by exactly 1"
    );

    println!("✓ Transaction count test passed!");
}
