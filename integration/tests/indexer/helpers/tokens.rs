use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    program_pack::Pack,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};
use solana_system_interface::instruction::create_account;
use spl_associated_token_account::{
    get_associated_token_address_with_program_id,
    instruction::create_associated_token_account_idempotent,
};
use spl_token::{
    instruction::{initialize_mint, mint_to},
    state::Mint as TokenMint,
};

use super::transactions::send_and_confirm_instructions;

pub async fn generate_mint(
    client: &RpcClient,
    payer: &Keypair,
    authority: &Keypair,
    mint: &Keypair,
) -> Result<Pubkey, Box<dyn std::error::Error>> {
    let space = TokenMint::LEN;
    let rent = client.get_minimum_balance_for_rent_exemption(space).await?;

    let instructions = vec![
        create_account(
            &payer.pubkey(),
            &mint.pubkey(),
            rent,
            space as u64,
            &spl_token::id(),
        ),
        initialize_mint(
            &spl_token::id(),
            &mint.pubkey(),
            &authority.pubkey(),
            Some(&authority.pubkey()),
            6, // decimals
        )?,
    ];

    send_and_confirm_instructions(
        client,
        &instructions,
        payer,
        &[payer, mint],
        "Generate Mint",
    )
    .await?;

    Ok(mint.pubkey())
}

#[allow(dead_code)]
pub async fn mint_to_owner(
    client: &RpcClient,
    payer: &Keypair,
    mint: Pubkey,
    owner: Pubkey,
    authority: &Keypair,
    amount: u64,
) -> Result<Pubkey, Box<dyn std::error::Error>> {
    let ata = get_associated_token_address_with_program_id(&owner, &mint, &spl_token::id());

    let instructions = vec![
        create_associated_token_account_idempotent(
            &payer.pubkey(),
            &owner,
            &mint,
            &spl_token::id(),
        ),
        mint_to(
            &spl_token::id(),
            &mint,
            &ata,
            &authority.pubkey(),
            &[],
            amount,
        )?,
    ];

    send_and_confirm_instructions(
        client,
        &instructions,
        payer,
        &[payer, authority],
        "Mint to Owner",
    )
    .await?;

    Ok(ata)
}

#[allow(unused)]
/// Get token account balance for a specific user and mint
pub async fn get_token_balance(
    client: &RpcClient,
    owner: &Pubkey,
    mint: &Pubkey,
) -> Result<u64, Box<dyn std::error::Error>> {
    let ata = get_associated_token_address_with_program_id(owner, mint, &spl_token::id());

    Ok(client
        .get_token_account_balance(&ata)
        .await?
        .amount
        .parse::<u64>()?)
}
