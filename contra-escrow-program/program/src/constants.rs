use crate::ID as CONTRA_ESCROW_PROGRAM_ID;
use const_crypto::ed25519;
use pinocchio::pubkey::Pubkey;

// Seeds
pub const INSTANCE_SEED: &[u8] = b"instance";
pub const OPERATOR_SEED: &[u8] = b"operator";
pub const ALLOWED_MINT_SEED: &[u8] = b"allowed_mint";

// Instance
pub const INSTANCE_VERSION: u8 = 1;

#[cfg(not(feature = "test-tree"))]
pub mod tree_constants {
    pub const TREE_HEIGHT: usize = 16;
    pub const MAX_TREE_LEAVES: usize = 2_usize.pow(16);

    pub const EMPTY_TREE_ROOT: [u8; 32] = [
        143, 230, 177, 104, 146, 86, 192, 211, 133, 244, 47, 91, 190, 32, 39, 162, 44, 25, 150,
        225, 16, 186, 151, 193, 113, 211, 229, 148, 141, 233, 43, 235,
    ];
}

// 8 leaves for testing
#[cfg(feature = "test-tree")]
pub mod tree_constants {
    pub const TREE_HEIGHT: usize = 3;
    pub const MAX_TREE_LEAVES: usize = 2_usize.pow(3);

    pub const EMPTY_TREE_ROOT: [u8; 32] = [
        199, 128, 9, 253, 240, 127, 197, 106, 17, 241, 34, 55, 6, 88, 163, 83, 170, 165, 66, 237,
        99, 228, 76, 75, 193, 95, 244, 205, 16, 90, 179, 60,
    ];
}

// This is the leaf value of a non-present nonce (empty leaf)
pub const EMPTY_LEAF: [u8; 32] = [0u8; 32];

// This is the leaf value of a present nonce (non-empty leaf)
pub const NON_EMPTY_LEAF_HASH: [u8; 32] = const_crypto::sha2::Sha256::new()
    .update(&[1u8; 32])
    .finalize();

// Seeds and PDAs
pub const EVENT_AUTHORITY_SEED: &[u8] = b"event_authority";

// Anchor Compatitable Discriminator: Sha256(anchor:event)[..8]
pub const EVENT_IX_TAG: u64 = 0x1d9acb512ea545e4;
pub const EVENT_IX_TAG_LE: &[u8] = EVENT_IX_TAG.to_le_bytes().as_slice();

// Event Authority PDA
pub mod event_authority_pda {

    use super::*;

    const EVENT_AUTHORITY_AND_BUMP: ([u8; 32], u8) =
        ed25519::derive_program_address(&[EVENT_AUTHORITY_SEED], &CONTRA_ESCROW_PROGRAM_ID);

    pub const ID: Pubkey = EVENT_AUTHORITY_AND_BUMP.0;
    pub const BUMP: u8 = EVENT_AUTHORITY_AND_BUMP.1;
}
