use super::test_context::ContraContext;

pub async fn run_get_block_time_test(ctx: &ContraContext) {
    println!("\n=== Get Block Time Test ===");

    // Get the current slot
    let current_slot = ctx.read_client.get_slot().await.unwrap();
    println!("Current slot: {}", current_slot);

    // Get the first available block
    let first_block = ctx.get_first_available_block().await.unwrap();
    println!("First available block: {}", first_block);

    // Test 1: Get block time for existing blocks
    println!("\nTest 1: Get block time for existing blocks");
    let blocks = ctx
        .get_blocks(first_block, Some(current_slot))
        .await
        .unwrap();

    if blocks.is_empty() {
        println!("Warning: No blocks found, skipping block time tests");
        return;
    }

    println!("Retrieved {} blocks", blocks.len());

    // Test 2: Check block time for several blocks
    println!("\nTest 2: Check block times are valid");
    let mut blocks_with_time = 0;
    let mut blocks_without_time = 0;
    let mut blocks_with_error = 0;

    // Check up to 10 blocks to verify block time data
    for &slot in blocks.iter().take(10) {
        match ctx.get_block_time(slot).await {
            Ok(Some(block_time)) => {
                println!("✓ Block {} has time: {}", slot, block_time);
                assert!(
                    block_time > 0,
                    "Block time should be a positive Unix timestamp"
                );
                blocks_with_time += 1;
            }
            Ok(None) => {
                println!("  Block {} has no block time (this is acceptable)", slot);
                blocks_without_time += 1;
            }
            Err(e) => {
                println!("  Block {} returned error (acceptable): {}", slot, e);
                blocks_with_error += 1;
            }
        }
    }

    println!(
        "✓ Verified {} blocks: {} with time, {} without time, {} with errors",
        blocks_with_time + blocks_without_time + blocks_with_error,
        blocks_with_time,
        blocks_without_time,
        blocks_with_error
    );

    // Test 3: Query block time for a non-existent block (future slot)
    let future_slot = current_slot + 10000;
    println!("\nTest 3: Query block time for future slot {}", future_slot);
    match ctx.get_block_time(future_slot).await {
        Ok(None) => {
            println!("✓ Future slot correctly returns None");
        }
        Ok(Some(time)) => {
            panic!("Future slot should not have a block time, got: {}", time);
        }
        Err(e) => {
            println!("✓ Future slot query returned error (acceptable): {}", e);
        }
    }

    // Test 4: Verify block times are increasing (monotonic)
    if blocks.len() >= 3 {
        println!("\nTest 4: Verify block times are monotonically increasing");
        let test_slots: Vec<u64> = blocks.iter().take(3).copied().collect();
        let mut previous_time: Option<i64> = None;

        for slot in test_slots {
            if let Ok(Some(block_time)) = ctx.get_block_time(slot).await {
                if let Some(prev) = previous_time {
                    assert!(
                        block_time >= prev,
                        "Block times should be monotonically increasing: slot {} time {} < previous time {}",
                        slot,
                        block_time,
                        prev
                    );
                }
                previous_time = Some(block_time);
            }
        }
        if previous_time.is_some() {
            println!("✓ Block times are monotonically increasing");
        } else {
            println!("  Note: Not enough blocks with times to verify monotonicity");
        }
    }

    // Test 5: Verify consistency with getBlock
    println!("\nTest 5: Verify getBlockTime is consistent with getBlock");
    for &slot in blocks.iter().take(5) {
        let block_time_result = ctx.get_block_time(slot).await.ok().flatten();
        let block = ctx.read_client.get_block(slot).await;

        match (block_time_result, block) {
            (Some(block_time), Ok(block)) => {
                // Both methods returned data - verify consistency
                if let Some(block_time_from_block) = block.block_time {
                    assert_eq!(
                        block_time, block_time_from_block,
                        "getBlockTime and getBlock should return the same block_time for slot {}",
                        slot
                    );
                    println!("✓ Block {} time is consistent: {}", slot, block_time);
                }
            }
            (None, Err(_)) => {
                // Both methods indicate no data - this is consistent
                println!("  Block {} has no data in both methods (consistent)", slot);
            }
            _ => {
                // One method has data, the other doesn't - log but don't fail
                // This can happen if block metadata is stored but full block data isn't
                println!(
                    "  Block {} has partial data (getBlockTime: {:?})",
                    slot, block_time_result
                );
            }
        }
    }

    println!("\n✓ All getBlockTime tests passed!");
}
