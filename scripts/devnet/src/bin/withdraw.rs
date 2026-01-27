use contra_withdraw_program_client::instructions::{WithdrawFunds, WithdrawFundsInstructionArgs};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    pubkey::Pubkey,
    signature::{read_keypair_file, Signer},
    transaction::Transaction,
};
use spl_associated_token_account::get_associated_token_address;
use std::{env, error::Error, str::FromStr};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 5 {
        eprintln!("Usage: {} <contra-gateway-rpc> <user-keypair-path> <mint-address> <amount> [destination]", args[0]);
        eprintln!("Example: {} http://localhost:8898 ./keypairs/user.json PANskKbAxqUQuqVfwzMtkyxii5GLaG1VFmB9xWb5tTP 500000", args[0]);
        eprintln!("\n⚠️  IMPORTANT: RPC URL must be Contra gateway (NOT Solana devnet)");
        eprintln!("  - Local:  http://localhost:8898");
        eprintln!("  - Docker: gateway:8899");
        eprintln!(
            "\nThis burns tokens on Contra. The operator will then release funds on Solana L1."
        );
        std::process::exit(1);
    }

    let rpc_url = &args[1];
    let keypair_path = &args[2];
    let mint = Pubkey::from_str(&args[3])?;
    let amount: u64 = args[4].parse()?;
    let destination = if args.len() > 5 {
        Some(Pubkey::from_str(&args[5])?)
    } else {
        None
    };

    println!("🔥 Withdrawing from Contra (burning tokens)");
    println!("Connecting to Contra gateway: {}", rpc_url);
    println!("Using user keypair: {}", keypair_path);
    println!("Mint: {}", mint);
    println!("Amount: {}", amount);
    if let Some(dest) = destination {
        println!("Destination (Solana L1): {}", dest);
    } else {
        println!("Destination: Same as user (default)");
    }

    let client = RpcClient::new(rpc_url.to_string());
    let user_keypair =
        read_keypair_file(keypair_path).map_err(|e| format!("Failed to read keypair: {}", e))?;

    println!("User pubkey: {}", user_keypair.pubkey());

    let user_ata = get_associated_token_address(&user_keypair.pubkey(), &mint);

    println!("\n📍 Transaction details:");
    println!("User ATA (on Contra): {}", user_ata);

    let instruction = WithdrawFunds {
        user: user_keypair.pubkey(),
        mint,
        token_account: user_ata,
        token_program: spl_token::ID,
        associated_token_program: spl_associated_token_account::ID,
    }
    .instruction(WithdrawFundsInstructionArgs {
        amount,
        destination,
    });

    let recent_blockhash = client.get_latest_blockhash()?;
    let transaction = Transaction::new_signed_with_payer(
        &[instruction],
        Some(&user_keypair.pubkey()),
        &[&user_keypair],
        recent_blockhash,
    );

    println!("Sending transaction...");
    let signature = client.send_and_confirm_transaction(&transaction)?;

    println!("\n✅ Withdrawal initiated on Contra!");
    println!("Transaction signature: {}", signature);
    println!("Burned {} tokens on Contra", amount);

    Ok(())
}
