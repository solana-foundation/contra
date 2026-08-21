use solana_sdk::pubkey::Pubkey;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

const SPL_INITIALIZE_MINT: u8 = 0;

/// A lazy-initialized static mapping from program_id (Pubkey) to a HashSet of admin instruction types (u8)
pub static ADMIN_INSTRUCTIONS_MAP: LazyLock<HashMap<Pubkey, HashSet<u8>>> =
    LazyLock::new(|| HashMap::from([(spl_token::id(), HashSet::from([SPL_INITIALIZE_MINT]))]));

/// Checks if an instruction is an admin-only instruction
pub fn is_admin_instruction(program_id: &Pubkey, instruction_type: u8) -> bool {
    ADMIN_INSTRUCTIONS_MAP
        .get(program_id)
        .is_some_and(|set| set.contains(&instruction_type))
}

// TODO: Make this configurable at startup
/// Checks if an instruction is allowed. Currently, only SPL instructions are
/// allowed
pub fn is_allowed_instruction(program_id: &Pubkey, _instruction_type: u8) -> bool {
    program_id == &spl_token::id()
}

/// bincode encodes a SystemInstruction variant tag as a 4-byte little-endian u32.
const SYSTEM_TRANSFER_DISCRIMINANT: u32 = 2;

/// Reads the leading variant tag; data shorter than 4 bytes yields None.
fn system_discriminant(data: &[u8]) -> Option<u32> {
    data.get(..4)?.try_into().ok().map(u32::from_le_bytes)
}

/// Admission policy for a single top-level instruction.
///
/// System is restricted to Transfer because it is all any flow here needs, and
/// because CreateAccount, Allocate, and their seeded forms let a caller allocate
/// permanent state that the gasless model never charges for. We match the raw
/// variant tag rather than deserialize, so no decoder runs on attacker bytes at
/// ingress, and bincode reads that same tag first anyway.
pub fn is_allowed_program_instruction(program_id: &Pubkey, data: &[u8]) -> bool {
    if *program_id == solana_sdk_ids::system_program::ID {
        return system_discriminant(data) == Some(SYSTEM_TRANSFER_DISCRIMINANT);
    }
    *program_id == spl_token::id()
        || *program_id == spl_associated_token_account::id()
        || *program_id == spl_memo::id()
        || *program_id
            == private_channel_withdraw_program_client::PRIVATE_CHANNEL_WITHDRAW_PROGRAM_ID
        || *program_id == dvp_swap_program_client::DVP_SWAP_PROGRAM_ID
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_system_interface::instruction::SystemInstruction;

    #[test]
    fn spl_initialize_mint_is_admin() {
        assert!(is_admin_instruction(&spl_token::id(), 0));
    }

    #[test]
    fn spl_transfer_is_not_admin() {
        // SPL token transfer = instruction type 3
        assert!(!is_admin_instruction(&spl_token::id(), 3));
    }

    #[test]
    fn unknown_program_is_not_admin() {
        let random = Pubkey::new_unique();
        assert!(!is_admin_instruction(&random, 0));
    }

    #[test]
    fn test_is_allowed_instruction_spl_token() {
        // SPL token transfer (type 3) should be allowed
        assert!(is_allowed_instruction(&spl_token::id(), 3));
        // SPL token initialize mint (type 0) should also be allowed
        assert!(is_allowed_instruction(&spl_token::id(), 0));
    }

    #[test]
    fn test_is_allowed_instruction_unknown() {
        let random = Pubkey::new_unique();
        assert!(!is_allowed_instruction(&random, 0));
        assert!(!is_allowed_instruction(&random, 3));
    }

    // ── Admission policy (`is_allowed_program_instruction`) ──────────────────

    fn system_id() -> Pubkey {
        solana_sdk_ids::system_program::ID
    }

    fn encode(ix: &SystemInstruction) -> Vec<u8> {
        bincode::serialize(ix).unwrap()
    }

    /// A1: Transfer is the only System variant any flow here needs.
    #[test]
    fn system_transfer_is_admitted() {
        let data = encode(&SystemInstruction::Transfer { lamports: 1 });
        assert!(is_allowed_program_instruction(&system_id(), &data));
    }

    // A2: serialize the real enum rather than hand-written tag bytes so an
    // upstream variant reorder fails this test instead of silently reopening
    // the allocation surface.
    #[test]
    fn every_non_transfer_system_variant_is_rejected() {
        let owner = Pubkey::new_unique();
        let base = Pubkey::new_unique();
        let cases: [(&str, SystemInstruction); 12] = [
            (
                "CreateAccount",
                SystemInstruction::CreateAccount {
                    lamports: 0,
                    space: 10 * 1024 * 1024,
                    owner,
                },
            ),
            ("Assign", SystemInstruction::Assign { owner }),
            (
                "CreateAccountWithSeed",
                SystemInstruction::CreateAccountWithSeed {
                    base,
                    seed: "seed".to_string(),
                    lamports: 0,
                    space: 10 * 1024 * 1024,
                    owner,
                },
            ),
            (
                "AdvanceNonceAccount",
                SystemInstruction::AdvanceNonceAccount,
            ),
            (
                "WithdrawNonceAccount",
                SystemInstruction::WithdrawNonceAccount(1),
            ),
            (
                "InitializeNonceAccount",
                SystemInstruction::InitializeNonceAccount(owner),
            ),
            (
                "AuthorizeNonceAccount",
                SystemInstruction::AuthorizeNonceAccount(owner),
            ),
            (
                "Allocate",
                SystemInstruction::Allocate {
                    space: 10 * 1024 * 1024,
                },
            ),
            (
                "AllocateWithSeed",
                SystemInstruction::AllocateWithSeed {
                    base,
                    seed: "seed".to_string(),
                    space: 10 * 1024 * 1024,
                    owner,
                },
            ),
            (
                "AssignWithSeed",
                SystemInstruction::AssignWithSeed {
                    base,
                    seed: "seed".to_string(),
                    owner,
                },
            ),
            (
                "TransferWithSeed",
                SystemInstruction::TransferWithSeed {
                    lamports: 1,
                    from_seed: "seed".to_string(),
                    from_owner: owner,
                },
            ),
            (
                "UpgradeNonceAccount",
                SystemInstruction::UpgradeNonceAccount,
            ),
        ];

        for (name, ix) in cases {
            let data = encode(&ix);
            assert!(
                !is_allowed_program_instruction(&system_id(), &data),
                "System::{name} must be rejected at ingress"
            );
        }
    }

    /// A3: data shorter than the 4-byte tag fails closed.
    #[test]
    fn system_data_shorter_than_tag_is_rejected() {
        for data in [&[][..], &[2][..], &[2, 0, 0][..]] {
            assert!(
                !is_allowed_program_instruction(&system_id(), data),
                "truncated System data {data:?} must be rejected"
            );
        }
    }

    // A4: admission reads only the tag; the SVM stays the authoritative decoder
    // and fails a malformed tag-2 body with InvalidInstructionData, allocating
    // nothing.
    #[test]
    fn system_transfer_tag_admits_regardless_of_body() {
        assert!(is_allowed_program_instruction(&system_id(), &[2, 0, 0, 0]));

        let mut with_junk = vec![2, 0, 0, 0];
        with_junk.extend_from_slice(&1u64.to_le_bytes());
        with_junk.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
        assert!(is_allowed_program_instruction(&system_id(), &with_junk));
    }

    /// A5: the tag is little-endian; a big-endian 2 is a different variant.
    #[test]
    fn system_big_endian_two_is_rejected() {
        assert!(!is_allowed_program_instruction(&system_id(), &[0, 0, 0, 2]));
    }

    /// A6: non-System allowlisted programs are admitted for any instruction data.
    #[test]
    fn allowlisted_programs_admit_any_data() {
        let programs = [
            spl_token::id(),
            spl_associated_token_account::id(),
            spl_memo::id(),
            private_channel_withdraw_program_client::PRIVATE_CHANNEL_WITHDRAW_PROGRAM_ID,
            dvp_swap_program_client::DVP_SWAP_PROGRAM_ID,
        ];
        for program_id in programs {
            for data in [&[][..], &[0xff; 8][..]] {
                assert!(
                    is_allowed_program_instruction(&program_id, data),
                    "allowlisted program {program_id} must stay admitted"
                );
            }
        }
    }

    /// A7: an unknown program is rejected whatever the data.
    #[test]
    fn unknown_program_is_rejected() {
        let random = Pubkey::new_unique();
        for data in [&[][..], &[2, 0, 0, 0][..], &[0xff; 8][..]] {
            assert!(!is_allowed_program_instruction(&random, data));
        }
    }
}
