use crate::utils::CONTRA_ESCROW_PROGRAM_ID;
use solana_sdk::pubkey::Pubkey;

const INSTANCE_SEED: &[u8] = b"instance";
const EVENT_AUTHORITY_SEED: &[u8] = b"event_authority";
const ALLOWED_MINT_SEED: &[u8] = b"allowed_mint";
const OPERATOR_SEED: &[u8] = b"operator";

pub fn find_instance_pda(instance_seed: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[INSTANCE_SEED, instance_seed.as_ref()],
        &CONTRA_ESCROW_PROGRAM_ID,
    )
}

pub fn find_event_authority_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[EVENT_AUTHORITY_SEED], &CONTRA_ESCROW_PROGRAM_ID)
}

pub fn find_allowed_mint_pda(instance_pda: &Pubkey, mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[ALLOWED_MINT_SEED, instance_pda.as_ref(), mint.as_ref()],
        &CONTRA_ESCROW_PROGRAM_ID,
    )
}

pub fn find_operator_pda(instance_pda: &Pubkey, wallet: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[OPERATOR_SEED, instance_pda.as_ref(), wallet.as_ref()],
        &CONTRA_ESCROW_PROGRAM_ID,
    )
}
