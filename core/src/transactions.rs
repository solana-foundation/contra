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
