use {
    solana_hash::Hash,
    solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer, transaction::Transaction},
    spl_associated_token_account::get_associated_token_address,
    std::{fs, path::PathBuf},
};

const EVENT_AUTHORITY_SEED: &[u8] = b"event_authority";

fn find_withdraw_event_authority_pda() -> Pubkey {
    Pubkey::find_program_address(
        &[EVENT_AUTHORITY_SEED],
        &contra_withdraw_program_client::CONTRA_WITHDRAW_PROGRAM_ID,
    )
    .0
}

#[derive(Debug)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RPC error {}: {}", self.code, self.message)
    }
}

impl std::error::Error for RpcError {}

/// Create an SPL token transfer transaction
pub fn create_spl_transfer(
    from: &Keypair,
    to: &Pubkey,
    mint: &Pubkey,
    amount: u64,
    blockhash: Hash,
) -> Transaction {
    let from_pubkey = from.pubkey();
    let from_token_account = get_associated_token_address(&from_pubkey, mint);
    let to_token_account = get_associated_token_address(to, mint);

    Transaction::new_signed_with_payer(
        &[spl_token::instruction::transfer(
            &spl_token::id(),
            &from_token_account,
            &to_token_account,
            &from_pubkey,
            &[],
            amount,
        )
        .unwrap()],
        Some(&from_pubkey),
        &[from],
        blockhash,
    )
}

/// Create an SPL token burn transaction
pub fn create_spl_burn(from: &Keypair, mint: &Pubkey, amount: u64, blockhash: Hash) -> Transaction {
    let from_pubkey = from.pubkey();
    let from_token_account = get_associated_token_address(&from_pubkey, mint);

    Transaction::new_signed_with_payer(
        &[spl_token::instruction::burn(
            &spl_token::id(),
            &from_token_account,
            mint,
            &from_pubkey,
            &[],
            amount,
        )
        .unwrap()],
        Some(&from_pubkey),
        &[from],
        blockhash,
    )
}

/// Create a withdraw funds transaction (burns tokens and logs the event)
pub fn create_withdraw_funds(
    from: &Keypair,
    mint: &Pubkey,
    amount: u64,
    blockhash: Hash,
) -> Transaction {
    use contra_withdraw_program_client::instructions::WithdrawFundsBuilder;

    let from_pubkey = from.pubkey();
    let token_account = get_associated_token_address(&from_pubkey, mint);
    let event_authority = find_withdraw_event_authority_pda();

    let withdraw_ix = WithdrawFundsBuilder::new()
        .user(from_pubkey)
        .mint(*mint)
        .token_account(token_account)
        .token_program(spl_token::id())
        .associated_token_program(spl_associated_token_account::id())
        .event_authority(event_authority)
        .contra_withdraw_program(contra_withdraw_program_client::CONTRA_WITHDRAW_PROGRAM_ID)
        .amount(amount)
        .instruction();

    Transaction::new_signed_with_payer(&[withdraw_ix], Some(&from_pubkey), &[from], blockhash)
}

/// Create an admin transaction to initialize a mint
pub fn create_admin_initialize_mint(
    admin: &Keypair,
    mint: &Pubkey,
    decimals: u8,
    blockhash: Hash,
) -> Transaction {
    Transaction::new_signed_with_payer(
        &[spl_token::instruction::initialize_mint(
            &spl_token::id(),
            mint,
            &admin.pubkey(), // mint authority
            None,            // freeze authority
            decimals,
        )
        .unwrap()],
        Some(&admin.pubkey()),
        &[admin],
        blockhash,
    )
}

/// Create an admin transaction to mint tokens
pub fn create_admin_mint_to(
    admin: &Keypair,
    mint: &Pubkey,
    destination: &Pubkey,
    amount: u64,
    blockhash: Hash,
) -> Transaction {
    Transaction::new_signed_with_payer(
        &[spl_token::instruction::mint_to(
            &spl_token::id(),
            mint,
            destination,
            &admin.pubkey(),
            &[],
            amount,
        )
        .unwrap()],
        Some(&admin.pubkey()),
        &[admin],
        blockhash,
    )
}

/// Create a transaction to create an associated token account
pub fn create_ata_transaction(
    payer: &Keypair,
    owner: &Pubkey,
    mint: &Pubkey,
    blockhash: Hash,
) -> Transaction {
    Transaction::new_signed_with_payer(
        &[
            spl_associated_token_account::instruction::create_associated_token_account(
                &payer.pubkey(),
                owner,
                mint,
                &spl_token::id(),
            ),
        ],
        Some(&payer.pubkey()),
        &[payer],
        blockhash,
    )
}

/// Load a keypair from a file
pub fn load_keypair(path: &PathBuf) -> Result<Keypair, Box<dyn std::error::Error + Send + Sync>> {
    let keypair_string = fs::read_to_string(path)?;

    // Try to parse as JSON array of bytes
    let keypair_bytes: Vec<u8> = serde_json::from_str(&keypair_string)
        .map_err(|e| format!("Failed to parse keypair JSON: {}", e))?;

    // Solana keypairs are 64 bytes (32 bytes secret + 32 bytes public)
    if keypair_bytes.len() != 64 {
        return Err(format!(
            "Invalid keypair length: expected 64 bytes, got {}",
            keypair_bytes.len()
        )
        .into());
    }

    Keypair::try_from(keypair_bytes.as_slice())
        .map_err(|e| format!("Failed to create keypair: {}", e).into())
}
