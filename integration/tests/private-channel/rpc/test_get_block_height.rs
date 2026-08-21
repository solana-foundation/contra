use super::test_context::PrivateChannelContext;

pub async fn run_get_block_height_test(ctx: &PrivateChannelContext) {
    println!("\n=== Block Height Test ===");

    // Read a slot either side of the height so the comparison survives block production.
    let slot_before = ctx.get_slot().await.unwrap();
    let block_height = ctx.get_block_height().await.unwrap();
    let slot_after = ctx.get_slot().await.unwrap();
    println!(
        "Slot before: {}, block height: {}, slot after: {}",
        slot_before, block_height, slot_after
    );

    // Block height is the slot here, so it must land inside the slots read around it.
    assert!(
        slot_before <= block_height && block_height <= slot_after,
        "Block height {} fell outside the slot range [{}, {}] read around it",
        block_height,
        slot_before,
        slot_after
    );

    println!("✓ Block height test passed!");
}
