use jsonrpsee::core::RpcResult;
use solana_epoch_schedule::EpochSchedule;

pub async fn get_epoch_schedule_impl() -> RpcResult<EpochSchedule> {
    // PrivateChannel has one massive epoch with u64::MAX slots and no warmup period
    // There is exactly one leader, so no leader schedule slot offset is needed
    Ok(EpochSchedule {
        slots_per_epoch: u64::MAX,
        leader_schedule_slot_offset: 0,
        warmup: false,
        first_normal_epoch: 0,
        first_normal_slot: 0,
    })
}
