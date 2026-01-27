use {
    super::types::ConflictFreeBatch, enum_dispatch::enum_dispatch,
    solana_sdk::transaction::SanitizedTransaction,
};

#[enum_dispatch]
pub trait SchedulerTrait {
    fn schedule(&mut self, transactions: Vec<SanitizedTransaction>) -> Vec<ConflictFreeBatch>;
}
