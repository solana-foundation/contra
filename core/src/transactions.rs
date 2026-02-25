use solana_sdk::pubkey::Pubkey;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

const SPL_INITIALIZE_MINT: u8 = 0;

/// A lazy-initialized static mapping from program_id (Pubkey) to a HashSet of admin instruction types (u8)
pub static ADMIN_INSTRUCTIONS_MAP: LazyLock<HashMap<Pubkey, HashSet<u8>>> = LazyLock::new(|| {
    let mut map = HashMap::new();
    // Add SPL Token admin instructions
    map.insert(spl_token::id(), {
        let mut set = HashSet::new();
        set.insert(SPL_INITIALIZE_MINT);
        set
    });
    map
});

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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn spl_token_is_allowed() {
        assert!(is_allowed_instruction(&spl_token::id(), 3));
    }

    #[test]
    fn unknown_program_is_not_allowed() {
        let random = Pubkey::new_unique();
        assert!(!is_allowed_instruction(&random, 0));
    }

    #[test]
    fn system_program_is_not_allowed() {
        assert!(!is_allowed_instruction(
            &solana_sdk::system_program::id(),
            0
        ));
    }
}
