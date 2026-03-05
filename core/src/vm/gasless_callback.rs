use crate::accounts::bob::BOB;
use solana_sdk::{
    account::{AccountSharedData, ReadableAccount},
    pubkey::Pubkey,
};
use solana_svm_callback::{InvokeContextCallback, TransactionProcessingCallback};
use std::collections::HashSet;

const DEFAULT_FEE_PAYER_LAMPORTS: u64 = 10;

pub struct GaslessCallback<'a> {
    bob: &'a BOB,
    fee_payers: HashSet<Pubkey>,
}

impl<'a> GaslessCallback<'a> {
    pub fn new(accounts_db: &'a BOB, fee_payers: HashSet<Pubkey>) -> Self {
        Self {
            bob: accounts_db,
            fee_payers,
        }
    }
}

impl<'a> InvokeContextCallback for GaslessCallback<'a> {}

impl<'a> TransactionProcessingCallback for GaslessCallback<'a> {
    fn get_account_shared_data(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
        self.bob.get_account_shared_data(pubkey).or_else(|| {
            self.fee_payers.contains(pubkey).then(|| {
                AccountSharedData::new(
                    DEFAULT_FEE_PAYER_LAMPORTS,
                    0,
                    &solana_sdk_ids::system_program::ID,
                )
            })
        })
    }

    fn account_matches_owners(
        &self,
        account: &solana_sdk::pubkey::Pubkey,
        owners: &[solana_sdk::pubkey::Pubkey],
    ) -> Option<usize> {
        self.get_account_shared_data(account)
            .and_then(|account| owners.iter().position(|key| account.owner().eq(key)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::create_test_bob;
    use solana_svm_callback::TransactionProcessingCallback;

    #[tokio::test]
    async fn test_fee_payer_returns_dummy_account() {
        let (bob, _tx) = create_test_bob();
        let fee_payer = Pubkey::new_unique();
        let cb = GaslessCallback::new(&bob, HashSet::from([fee_payer]));

        let account = cb.get_account_shared_data(&fee_payer).unwrap();
        assert_eq!(account.lamports(), DEFAULT_FEE_PAYER_LAMPORTS);
        assert_eq!(account.owner(), &solana_sdk_ids::system_program::ID);
    }

    #[tokio::test]
    async fn test_unknown_pubkey_returns_none() {
        let (bob, _tx) = create_test_bob();
        let cb = GaslessCallback::new(&bob, HashSet::new());

        assert!(cb.get_account_shared_data(&Pubkey::new_unique()).is_none());
    }

    #[tokio::test]
    async fn test_account_matches_owners_fee_payer() {
        let (bob, _tx) = create_test_bob();
        let fee_payer = Pubkey::new_unique();
        let cb = GaslessCallback::new(&bob, HashSet::from([fee_payer]));

        // Fee payer is owned by system program
        let system = solana_sdk_ids::system_program::ID;
        let other = Pubkey::new_unique();

        assert_eq!(
            cb.account_matches_owners(&fee_payer, &[other, system]),
            Some(1)
        );
        assert_eq!(cb.account_matches_owners(&fee_payer, &[other]), None);
    }

    #[tokio::test]
    async fn test_account_matches_owners_unknown() {
        let (bob, _tx) = create_test_bob();
        let cb = GaslessCallback::new(&bob, HashSet::new());

        let unknown = Pubkey::new_unique();
        assert_eq!(
            cb.account_matches_owners(&unknown, &[Pubkey::new_unique()]),
            None
        );
    }
}
