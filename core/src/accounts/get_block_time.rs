use super::traits::AccountsDB;

pub async fn get_block_time(db: &AccountsDB, slot: u64) -> Option<i64> {
    super::get_block::get_block(db, slot)
        .await
        .and_then(|b| b.block_time)
}
