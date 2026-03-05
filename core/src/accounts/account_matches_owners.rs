use {
    super::{get_account_shared_data::get_account_shared_data, traits::AccountsDB},
    solana_sdk::{account::ReadableAccount, pubkey::Pubkey},
};

pub async fn account_matches_owners(
    db: &AccountsDB,
    account: &Pubkey,
    owners: &[Pubkey],
) -> Option<usize> {
    let account_data = get_account_shared_data(db, account).await;
    account_data.and_then(|account| owners.iter().position(|key| account.owner().eq(key)))
}
