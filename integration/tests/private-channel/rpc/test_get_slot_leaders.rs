use super::test_context::PrivateChannelContext;

pub async fn run_get_slot_leaders_test(ctx: &PrivateChannelContext) {
    println!("\n=== Get Slot Leaders Test ===");

    // Get the current slot
    let current_slot = ctx.read_client.get_slot().await.unwrap();
    println!("Current slot: {}", current_slot);

    // Test 1: Get slot leaders starting from slot 0 with a limit of 100
    println!("\nTest 1: Get slot leaders from slot 0 with limit 100");
    let leaders = ctx.get_slot_leaders(0, 100).await.unwrap();
    println!("Retrieved {} slot leaders", leaders.len());

    // Verify that the result is empty (PrivateChannel has no slot leaders)
    assert_eq!(
        leaders.len(),
        0,
        "PrivateChannel should return empty array for slot leaders"
    );
    println!("✓ Correctly returned empty array for slot leaders");

    // Test 2: Get slot leaders starting from current slot with a limit of 10
    println!(
        "\nTest 2: Get slot leaders from current slot {} with limit 10",
        current_slot
    );
    let leaders = ctx.get_slot_leaders(current_slot, 10).await.unwrap();
    println!("Retrieved {} slot leaders", leaders.len());

    assert_eq!(
        leaders.len(),
        0,
        "PrivateChannel should return empty array for slot leaders"
    );
    println!("✓ Correctly returned empty array for slot leaders");

    // Test 3: Get slot leaders with large limit
    println!("\nTest 3: Get slot leaders with large limit (5000)");
    let leaders = ctx.get_slot_leaders(0, 5000).await.unwrap();
    println!("Retrieved {} slot leaders", leaders.len());

    assert_eq!(
        leaders.len(),
        0,
        "PrivateChannel should return empty array for slot leaders"
    );
    println!("✓ Correctly returned empty array for large limit");

    // Test 4: Get slot leaders from future slot
    let future_slot = current_slot + 1000;
    println!(
        "\nTest 4: Get slot leaders from future slot {}",
        future_slot
    );
    let leaders = ctx.get_slot_leaders(future_slot, 100).await.unwrap();
    println!("Retrieved {} slot leaders", leaders.len());

    assert_eq!(
        leaders.len(),
        0,
        "PrivateChannel should return empty array for slot leaders"
    );
    println!("✓ Correctly returned empty array for future slot");

    println!("\n✓ All getSlotLeaders tests passed!");
}
