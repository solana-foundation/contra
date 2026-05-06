//! # Fuzz harness — escrow core lifecycle
//!
//! Invariants tested:
//! - **Balance conservation**: `escrow_balance == total_deposited - total_released`
//! - **Invalid-proof rejection**: garbage proofs must fail without touching balances.
//! - **Double-spend prevention**: replaying a successful release must be rejected.

mod shared;

use std::collections::HashMap;

use private_channel_escrow_program_client::instructions::{DepositBuilder, ReleaseFundsBuilder};
use solana_sdk::compute_budget::ComputeBudgetInstruction;
use tests_private_channel_escrow_program::smt_utils::ProcessorSMT;
use trident_fuzz::fuzzing::*;

use shared::{clamp_amount, setup_escrow, token_amount, AccountAddresses};

/// Clamp nonces to [0, 999] — flat range, no tree-generation partitioning.
fn clamp_nonce(raw: u64) -> u64 {
    raw % 1_000
}

// ── State ─────────────────────────────────────────────────────────────────────

/// Everything needed to replay a previously successful release.
#[derive(Clone)]
struct SuccessfulRelease {
    amount: u64,
    new_withdrawal_root: [u8; 32],
    sibling_proofs: [u8; 512],
}

// ── Fuzz test ─────────────────────────────────────────────────────────────────

#[derive(Default, FuzzTestMethods)]
pub struct FuzzTest {
    pub trident: Trident,
    pub fuzz_accounts: AccountAddresses,
    /// Local mirror of the on-chain SMT.
    smt: ProcessorSMT,
    /// Successful releases keyed by nonce, for double-spend replay.
    successful_releases: HashMap<u64, SuccessfulRelease>,
    /// User's token balance at the start of the iteration (after minting).
    initial_user_balance: u64,
    total_deposited: u64,
    total_released: u64,
}

#[flow_executor]
impl FuzzTest {
    fn new() -> Self {
        Self::default()
    }

    #[init]
    fn start(&mut self) {
        self.initial_user_balance = setup_escrow(&mut self.trident, &mut self.fuzz_accounts);
        self.smt = ProcessorSMT::new();
        self.successful_releases.clear();
        self.total_deposited = 0;
        self.total_released = 0;
    }

    // ── Flows ─────────────────────────────────────────────────────────────────

    /// Deposit a random amount. Asserts exact ATA balance movement on success.
    #[flow]
    fn fuzz_deposit(&mut self) {
        let amount = clamp_amount(self.trident.random_from_range(1..u64::MAX));

        let user = self.fuzz_accounts.user.get(&mut self.trident).unwrap();
        let instance = self.fuzz_accounts.instance.get(&mut self.trident).unwrap();
        let mint = self.fuzz_accounts.mint.get(&mut self.trident).unwrap();
        let allowed_mint = self
            .fuzz_accounts
            .allowed_mint
            .get(&mut self.trident)
            .unwrap();
        let user_ata = self.fuzz_accounts.user_ata.get(&mut self.trident).unwrap();
        let instance_ata = self
            .fuzz_accounts
            .instance_ata
            .get(&mut self.trident)
            .unwrap();

        let instance_bal_before = token_amount(&mut self.trident, &instance_ata);
        let user_bal_before = token_amount(&mut self.trident, &user_ata);

        let ix = DepositBuilder::new()
            .payer(self.trident.payer().pubkey())
            .user(user)
            .instance(instance)
            .mint(mint)
            .allowed_mint(allowed_mint)
            .user_ata(user_ata)
            .instance_ata(instance_ata)
            .amount(amount)
            .instruction();

        let res = self.trident.process_transaction(&[ix], Some("deposit"));
        if res.is_success() {
            assert_eq!(
                token_amount(&mut self.trident, &instance_ata),
                instance_bal_before + amount
            );
            assert_eq!(
                token_amount(&mut self.trident, &user_ata),
                user_bal_before - amount
            );
            self.total_deposited = self.total_deposited.checked_add(amount).unwrap();
        }
    }

    /// 50% valid release / 50% garbage-proof release.
    ///
    /// Valid path: real exclusion proof — must succeed, balances must shift.
    /// Invalid path: garbage proofs — must fail, balances must be unchanged.
    #[flow]
    fn fuzz_release(&mut self) {
        let amount = clamp_amount(self.trident.random_from_range(1..u64::MAX));
        let nonce = clamp_nonce(self.trident.random_from_range(0..u64::MAX));
        let use_valid = self.trident.random_from_range(0..=1u8) == 0;

        let operator = self.fuzz_accounts.operator.get(&mut self.trident).unwrap();
        let instance = self.fuzz_accounts.instance.get(&mut self.trident).unwrap();
        let operator_pda = self
            .fuzz_accounts
            .operator_pda
            .get(&mut self.trident)
            .unwrap();
        let mint = self.fuzz_accounts.mint.get(&mut self.trident).unwrap();
        let allowed_mint = self
            .fuzz_accounts
            .allowed_mint
            .get(&mut self.trident)
            .unwrap();
        let user = self.fuzz_accounts.user.get(&mut self.trident).unwrap();
        let user_ata = self.fuzz_accounts.user_ata.get(&mut self.trident).unwrap();
        let instance_ata = self
            .fuzz_accounts
            .instance_ata
            .get(&mut self.trident)
            .unwrap();

        let instance_bal_before = token_amount(&mut self.trident, &instance_ata);
        let user_bal_before = token_amount(&mut self.trident, &user_ata);

        let (sibling_proofs, new_root, should_succeed) =
            if use_valid && !self.smt.contains(nonce) && amount <= instance_bal_before {
                let (_, proofs) = self.smt.generate_exclusion_proof(nonce);
                let mut next = self.smt.clone();
                next.insert(nonce);
                (proofs, next.current_root(), true)
            } else {
                ([0xddu8; 512], [0xffu8; 32], false)
            };

        // ReleaseFunds requires 1.2M CUs for SMT proof verification.
        let cu_ix = ComputeBudgetInstruction::set_compute_unit_limit(1_200_000);
        let ix = ReleaseFundsBuilder::new()
            .payer(self.trident.payer().pubkey())
            .operator(operator)
            .instance(instance)
            .operator_pda(operator_pda)
            .mint(mint)
            .allowed_mint(allowed_mint)
            .user_ata(user_ata)
            .instance_ata(instance_ata)
            .amount(amount)
            .user(user)
            .new_withdrawal_root(new_root)
            .transaction_nonce(nonce)
            .sibling_proofs(sibling_proofs)
            .instruction();

        let res = self
            .trident
            .process_transaction(&[cu_ix, ix], Some("release"));

        if should_succeed {
            assert!(
                res.is_success(),
                "valid release failed nonce={nonce} amount={amount}: {}",
                res.logs()
            );
            self.smt.insert(nonce);
            self.successful_releases.insert(
                nonce,
                SuccessfulRelease {
                    amount,
                    new_withdrawal_root: new_root,
                    sibling_proofs,
                },
            );
            assert_eq!(
                token_amount(&mut self.trident, &instance_ata),
                instance_bal_before - amount
            );
            assert_eq!(
                token_amount(&mut self.trident, &user_ata),
                user_bal_before + amount
            );
            self.total_released = self.total_released.checked_add(amount).unwrap();
        } else {
            assert!(
                !res.is_success(),
                "invalid release should fail nonce={nonce}"
            );
            assert_eq!(
                token_amount(&mut self.trident, &instance_ata),
                instance_bal_before,
                "instance balance changed on failed release"
            );
            assert_eq!(
                token_amount(&mut self.trident, &user_ata),
                user_bal_before,
                "user balance changed on failed release"
            );
        }
    }

    /// Replay an already-processed release with the exact same proof — must be rejected.
    #[flow]
    fn fuzz_double_spend(&mut self) {
        let nonce = clamp_nonce(self.trident.random_from_range(0..u64::MAX));
        let Some(prev) = self.successful_releases.get(&nonce).cloned() else {
            return;
        };

        let operator = self.fuzz_accounts.operator.get(&mut self.trident).unwrap();
        let instance = self.fuzz_accounts.instance.get(&mut self.trident).unwrap();
        let operator_pda = self
            .fuzz_accounts
            .operator_pda
            .get(&mut self.trident)
            .unwrap();
        let mint = self.fuzz_accounts.mint.get(&mut self.trident).unwrap();
        let allowed_mint = self
            .fuzz_accounts
            .allowed_mint
            .get(&mut self.trident)
            .unwrap();
        let user = self.fuzz_accounts.user.get(&mut self.trident).unwrap();
        let user_ata = self.fuzz_accounts.user_ata.get(&mut self.trident).unwrap();
        let instance_ata = self
            .fuzz_accounts
            .instance_ata
            .get(&mut self.trident)
            .unwrap();
        let instance_bal_before = token_amount(&mut self.trident, &instance_ata);
        let user_bal_before = token_amount(&mut self.trident, &user_ata);

        let cu_ix = ComputeBudgetInstruction::set_compute_unit_limit(1_200_000);
        let ix = ReleaseFundsBuilder::new()
            .payer(self.trident.payer().pubkey())
            .operator(operator)
            .instance(instance)
            .operator_pda(operator_pda)
            .mint(mint)
            .allowed_mint(allowed_mint)
            .user_ata(user_ata)
            .instance_ata(instance_ata)
            .amount(prev.amount)
            .user(user)
            .new_withdrawal_root(prev.new_withdrawal_root)
            .transaction_nonce(nonce)
            .sibling_proofs(prev.sibling_proofs)
            .instruction();

        let res = self
            .trident
            .process_transaction(&[cu_ix, ix], Some("double_spend"));
        assert!(
            !res.is_success(),
            "double-spend must be rejected: nonce={nonce}"
        );
        assert_eq!(
            token_amount(&mut self.trident, &instance_ata),
            instance_bal_before,
            "instance balance changed on double-spend"
        );
        assert_eq!(
            token_amount(&mut self.trident, &user_ata),
            user_bal_before,
            "user balance changed on double-spend"
        );
    }

    // ── Invariant ─────────────────────────────────────────────────────────────

    /// `escrow_balance == total_deposited - total_released`
    /// `user_balance == initial_user_balance - total_deposited + total_released`
    #[end]
    fn end(&mut self) {
        let instance_ata = self
            .fuzz_accounts
            .instance_ata
            .get(&mut self.trident)
            .unwrap();
        let user_ata = self.fuzz_accounts.user_ata.get(&mut self.trident).unwrap();

        let expected_instance = self
            .total_deposited
            .checked_sub(self.total_released)
            .expect("released more than deposited");
        assert_eq!(
            token_amount(&mut self.trident, &instance_ata),
            expected_instance,
            "final escrow balance mismatch: deposited={} released={}",
            self.total_deposited,
            self.total_released,
        );

        let expected_user = self
            .initial_user_balance
            .checked_sub(self.total_deposited)
            .and_then(|x| x.checked_add(self.total_released))
            .expect("user balance model overflow");
        assert_eq!(
            token_amount(&mut self.trident, &user_ata),
            expected_user,
            "final user balance mismatch: initial={} deposited={} released={}",
            self.initial_user_balance,
            self.total_deposited,
            self.total_released,
        );
    }
}

fn main() {
    FuzzTest::fuzz(1000, 32);
}
