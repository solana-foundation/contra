#[allow(deprecated)]
use solana_rent_collector::CollectedInfo;
use {
    solana_sdk::{
        account::AccountSharedData,
        clock::Epoch,
        pubkey::Pubkey,
        rent::{Rent, RentDue},
    },
    solana_svm_rent_collector::svm_rent_collector::SVMRentCollector,
};

/// A gasless rent collector that doesn't charge any rent or fees
/// We don't need to store a Rent struct since everything is free
#[derive(Debug, Default, Clone)]
pub struct GaslessRentCollector;

impl GaslessRentCollector {
    pub fn new() -> Self {
        Self
    }
}

impl SVMRentCollector for GaslessRentCollector {
    #[allow(deprecated)]
    fn collect_rent(&self, _address: &Pubkey, _account: &mut AccountSharedData) -> CollectedInfo {
        #[allow(deprecated)]
        CollectedInfo {
            rent_amount: 0,
            account_data_len_reclaimed: 0,
        }
    }

    fn get_rent(&self) -> &Rent {
        // Return a static zero rent for gasless operation
        // We use a static to avoid allocating on every call
        static ZERO_RENT: Rent = Rent {
            lamports_per_byte_year: 0,
            exemption_threshold: 0.0,
            burn_percent: 0,
        };
        &ZERO_RENT
    }

    fn get_rent_due(
        &self,
        _lamports: u64,
        _data_len: usize,
        _account_rent_epoch: Epoch,
    ) -> RentDue {
        // Gasless - no rent due ever
        RentDue::Exempt
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_sdk::account::ReadableAccount;
    use solana_svm_rent_collector::svm_rent_collector::SVMRentCollector;

    #[test]
    fn test_collect_rent_returns_zero() {
        let collector = GaslessRentCollector::new();
        let pubkey = Pubkey::new_unique();
        let mut account = AccountSharedData::new(1000, 100, &Pubkey::new_unique());

        #[allow(deprecated)]
        let info = collector.collect_rent(&pubkey, &mut account);
        assert_eq!(info.rent_amount, 0);
        assert_eq!(info.account_data_len_reclaimed, 0);
        // Account should be unchanged
        assert_eq!(account.lamports(), 1000);
    }

    #[test]
    fn test_get_rent_zero_values() {
        let collector = GaslessRentCollector::new();
        let rent = collector.get_rent();
        assert_eq!(rent.lamports_per_byte_year, 0);
        assert_eq!(rent.burn_percent, 0);
    }

    #[test]
    fn test_get_rent_due_always_exempt() {
        let collector = GaslessRentCollector::new();
        assert_eq!(collector.get_rent_due(0, 0, 0), RentDue::Exempt);
        assert_eq!(
            collector.get_rent_due(1_000_000, 1024, 100),
            RentDue::Exempt
        );
    }
}
