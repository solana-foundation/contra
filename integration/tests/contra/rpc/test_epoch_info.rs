use super::test_context::ContraContext;

pub async fn run_epoch_info_test(ctx: &ContraContext) {
    println!("\n=== Epoch Info Test ===");

    // Get epoch info
    let epoch_info = ctx.get_epoch_info().await.unwrap();
    let transaction_count = ctx.get_transaction_count().await.unwrap();
    println!("Epoch info: {:?}", epoch_info);

    assert!(
        epoch_info.absolute_slot == epoch_info.block_height,
        "Absolute slot should be equal to block height"
    );

    assert!(
        epoch_info.absolute_slot == epoch_info.slot_index,
        "Absolute slot should be equal to slot index"
    );

    assert!(epoch_info.epoch == 0, "Epoch should be 0");
    assert!(
        epoch_info.slots_in_epoch == u64::MAX,
        "Slots in epoch should be u64::MAX"
    );

    assert!(
        epoch_info.transaction_count.unwrap() == transaction_count,
        "Transaction count from getEpochInfo should be equal to the transaction count from getTransactionCount"
    );
}
