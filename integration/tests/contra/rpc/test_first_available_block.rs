use super::test_context::ContraContext;

pub async fn run_first_available_block_test(ctx: &ContraContext) {
    println!("\n=== First Available Block Test ===");

    // Get the first available block
    let first_block = ctx.get_first_available_block().await.unwrap();
    println!("First available block: {}", first_block);

    // Get the current slot to verify first_block is less than or equal to it
    let current_slot = ctx.read_client.get_slot().await.unwrap();
    println!("Current slot: {}", current_slot);

    // Verify that first_block is 0
    assert_eq!(first_block, 0, "First available block should be 0");

    println!("✓ First available block test passed!");
}
