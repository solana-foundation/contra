use super::test_context::PrivateChannelContext;

pub async fn run_first_available_block_test(ctx: &PrivateChannelContext) {
    println!("\n=== First Available Block Test ===");

    let first_block = ctx.get_first_available_block().await.unwrap();
    println!("First available block: {}", first_block);

    assert_eq!(first_block, 0, "First available block should be 0");

    println!("✓ First available block test passed!");
}
