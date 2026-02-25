use crate::ID as CONTRA_WITHDRAW_PROGRAM_ID;
use const_crypto::ed25519;
use pinocchio::pubkey::Pubkey;

pub const EVENT_AUTHORITY_SEED: &[u8] = b"event_authority";

// Anchor compatible discriminator: sha256("anchor:event")[..8]
pub const EVENT_IX_TAG: u64 = 0x1d9acb512ea545e4;
pub const EVENT_IX_TAG_LE: &[u8] = EVENT_IX_TAG.to_le_bytes().as_slice();

pub mod event_authority_pda {
    use super::*;

    const EVENT_AUTHORITY_AND_BUMP: ([u8; 32], u8) =
        ed25519::derive_program_address(&[EVENT_AUTHORITY_SEED], &CONTRA_WITHDRAW_PROGRAM_ID);

    pub const ID: Pubkey = EVENT_AUTHORITY_AND_BUMP.0;
    pub const BUMP: u8 = EVENT_AUTHORITY_AND_BUMP.1;
}
