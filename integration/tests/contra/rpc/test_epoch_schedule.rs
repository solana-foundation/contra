use super::test_context::ContraContext;

pub async fn run_epoch_schedule_test(ctx: &ContraContext) {
    println!("\n=== Epoch Schedule Test ===");

    // Get epoch schedule
    let epoch_schedule = ctx.get_epoch_schedule().await.unwrap();
    println!("Epoch schedule: {:?}", epoch_schedule);

    // Contra has one massive epoch with u64::MAX slots and no warmup period
    // There is exactly one leader, so no leader schedule slot offset is needed
    assert!(
        epoch_schedule.slots_per_epoch == u64::MAX,
        "Slots per epoch should be u64::MAX"
    );

    assert!(
        epoch_schedule.leader_schedule_slot_offset == 0,
        "Leader schedule slot offset should be 0"
    );

    assert!(!epoch_schedule.warmup, "Warmup should be false");

    assert!(
        epoch_schedule.first_normal_epoch == 0,
        "First normal epoch should be 0"
    );

    assert!(
        epoch_schedule.first_normal_slot == 0,
        "First normal slot should be 0"
    );
}
