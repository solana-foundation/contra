use super::test_context::ContraContext;

pub async fn run_epoch_info_test(ctx: &ContraContext) {
    println!("\n=== Epoch Info Test ===");

    let epoch_info = ctx.get_epoch_info().await.unwrap();
    let transaction_count = ctx.get_transaction_count().await.unwrap();
    println!("Epoch info: {:?}", epoch_info);

    assert_eq!(
        epoch_info.absolute_slot, epoch_info.block_height,
        "Absolute slot should be equal to block height"
    );
    assert_eq!(
        epoch_info.absolute_slot, epoch_info.slot_index,
        "Absolute slot should be equal to slot index"
    );
    assert_eq!(epoch_info.epoch, 0, "Epoch should be 0");
    assert_eq!(
        epoch_info.slots_in_epoch,
        u64::MAX,
        "Slots in epoch should be u64::MAX"
    );
    assert_eq!(
        epoch_info.transaction_count.unwrap(),
        transaction_count,
        "Transaction count from getEpochInfo should equal getTransactionCount"
    );
}
