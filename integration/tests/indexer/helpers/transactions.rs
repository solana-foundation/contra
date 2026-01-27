use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    instruction::Instruction,
    message::Message,
    signature::{Keypair, Signature, Signer},
    transaction::Transaction,
};
use solana_system_interface::instruction as system_instruction;

const COMPUTE_UNIT_LIMIT: u32 = 200_000;
const COMPUTE_UNIT_PRICE: u64 = 1;
const AIRDROP_AMOUNT: u64 = 10_000_000_000;

/// Send and confirm a transaction with instructions
pub async fn send_and_confirm_instructions(
    client: &RpcClient,
    instructions: &[Instruction],
    payer: &Keypair,
    signers: &[&Keypair],
    description: &str,
) -> Result<Signature, Box<dyn std::error::Error>> {
    // Add compute budget instructions
    let mut all_instructions = vec![
        ComputeBudgetInstruction::set_compute_unit_limit(COMPUTE_UNIT_LIMIT),
        ComputeBudgetInstruction::set_compute_unit_price(COMPUTE_UNIT_PRICE),
    ];
    all_instructions.extend_from_slice(instructions);

    // Get recent blockhash
    let recent_blockhash = client.get_latest_blockhash().await?;

    // Create message and transaction
    let message = Message::new(&all_instructions, Some(&payer.pubkey()));
    let mut transaction = Transaction::new_unsigned(message);
    transaction.sign(signers, recent_blockhash);

    // Send and confirm
    let signature = client
        .send_and_confirm_transaction(&transaction)
        .await
        .map_err(|e| format!("Failed to {}: {}", description.to_lowercase(), e))?;

    Ok(signature)
}

/// Setup wallets by airdropping SOL
pub async fn setup_wallets(
    client: &RpcClient,
    faucet_keypair: &Keypair,
    wallets: &[&Keypair],
) -> Result<(), Box<dyn std::error::Error>> {
    for wallet in wallets {
        let recent_blockhash = client.get_latest_blockhash().await?;
        let transfer_ix = system_instruction::transfer(
            &faucet_keypair.pubkey(),
            &wallet.pubkey(),
            AIRDROP_AMOUNT,
        );
        let tx = Transaction::new_signed_with_payer(
            &[transfer_ix],
            Some(&faucet_keypair.pubkey()),
            &[&faucet_keypair],
            recent_blockhash,
        );
        client.send_and_confirm_transaction(&tx).await?;

        println!(
            "Airdropped {} SOL to {}. New balance: {} SOL",
            AIRDROP_AMOUNT,
            wallet.pubkey(),
            client.get_balance(&wallet.pubkey()).await?
        );

        while client.get_balance(&wallet.pubkey()).await? < AIRDROP_AMOUNT {
            println!("Waiting for airdrop to complete for {}", wallet.pubkey());
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        }
    }

    Ok(())
}
