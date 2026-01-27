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
    fee_payers: HashSet<Pubkey>, // Own the Pubkeys instead of borrowing
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
        if let Some(account) = self.bob.get_account_shared_data(pubkey) {
            return Some(account);
        } else if self.fee_payers.contains(pubkey) {
            return Some(AccountSharedData::new(
                DEFAULT_FEE_PAYER_LAMPORTS,
                0,
                &solana_sdk_ids::system_program::ID, // Use system program as owner for fee payer accounts
            ));
        }
        None
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
