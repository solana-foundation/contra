# Devnet Quick Start Guide

This guide walks you through deploying and running Contra on Solana Devnet. By the end, you'll have a fully operational Contra payment channel with real-time monitoring and deposit/withdrawal access to Solana Devnet.

## What is Contra?

Contra is a private payment channel with direct access to Solana liquidity. Users deposit tokens into an escrow on Solana Mainnet (or Devnet), which mints equivalent tokens on the Contra payment channel. When withdrawing, tokens are burned on Contra and released from escrow on Solana.

**Architecture Overview:**

- **Escrow Program**: On-chain Solana program that holds deposited tokens (Devnet Program ID: `GokvZqD2yP696rzNBNbQvcZ4VsLW7jNvFXU1kW9m7k83`)
- **Contra Payment Channel**: Private execution environment (write node, read node, gateway)
- **Indexer** (2 instances): `indexer-solana` watches the Escrow program on Solana for deposits; `indexer-contra` watches the Withdraw program on the Contra payment channel for withdrawals
- **Operator** (2 instances): `operator-solana` processes deposits (mints on the Contra payment channel); `operator-contra` processes withdrawals (releases from escrow on Solana)

## Prerequisites

Before starting, ensure you have:

- **Docker** (Engine or Desktop)  
  - macOS Apple Silicon: Enable "Docker VMM" in Docker settings (configurable in "Settings" \-\> "Virtual Machine Options")  
- **Node.js** (v20+) and **pnpm**  
- **Solana CLI** (latest)  
- **Rust** (latest)  
- **Solana Wallet** that supports localhost or custom RPC (e.g., Backpack, Phantom, Solflare)  
- **Solana Devnet RPC** endpoint  
- **Yellowstone gRPC (Devnet)** endpoint (for real-time Solana event streaming)  
  - **Note**: You can use public Devnet RPC but need a Devnet Yellowstone gRPC node from a service provider for real-time indexing (e.g., Helius LazerStream, Triton, QuickNode).

## Step 1: Build Docker Images

From the project root:

```shell
docker compose -f docker-compose.devnet.yml build
```

This builds all Contra services (gateway, nodes, indexer, operator). This will take a long time (30min to an hour or so depending on your system), so it's recommended to run this in the background while you configure the rest of the stack (or go to the gym).

## Step 2: Set Up Admin UI

The Admin UI lets you create and configure your escrow instance via a web interface.

If you prefer, you can also use the [scripts](../scripts/devnet/README.md) or the [Escrow](../contra-escrow-program/clients/typescript) and [Withdrawal](../contra-withdraw-program/clients/typescript) clients to interact with the programs.

**Note:** The CLI scripts in `scripts/devnet/` may reference port 8898 for the gateway. This guide uses the Docker Compose default of 8899. Ensure your port configuration is consistent.

```shell
cd admin-ui
pnpm install
```

Create an environment file for the Admin UI:

```shell
# admin-ui/.env
CONTRA_RPC_URL=http://localhost:8899
```

Start the development server:

```shell
pnpm dev
```

Open [http://localhost:5173](http://localhost:5173) in your browser.

## Step 3: Create an Escrow Instance

1. **Connect Wallet**  
     
   - Set your browser wallet to **Devnet** network  
   - Ensure you have Devnet SOL for transaction fees (use the [Solana Faucet](https://faucet.solana.com/) if needed)

   

2. **Create Instance**  
     
   - In the Admin UI, click **"Create New Instance"**  
   - Approve the transaction in your wallet  
   - **Copy the Instance Address** — you'll need this for configuration

![Create Instance](./assets/create-instance.png)

## Step 4: Generate Operator Keypair

The operator keypair signs transactions for minting on the Contra payment channel and releasing from escrow.

```shell
# Generate a new keypair
solana-keygen new -o operator-keypair.json -s --no-bip39-passphrase

# Get the public key
solana-keygen pubkey operator-keypair.json
```

## Step 5: Configure the Instance

Back in the Admin UI:

### Whitelist a Token Mint

1. Go to **Admin Functions** → **Mint Management**  
2. Enter the mint address you want to support (e.g., Devnet USDC: `4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU` – you can get some at the [USDC Faucet](https://faucet.circle.com/) or use your own devnet token mint)  
3. Click **"Allow Mint"** and approve the transaction

![Allow Mint](./assets/allow-mint.png)

### Add Operator

1. Go to **Admin Functions** → **Operator Management**  
2. Enter your operator's public key (from Step 4)  
3. Click **"Add Operator"** and approve the transaction


## Step 6: Configure Environment Variables

Update `.env.devnet` file in the project root. Replace the following environment variables:

```shell
# Escrow instance (from Step 3)
ESCROW_INSTANCE_ID=<your_instance_address>

# Operator keypair (the contents of operator-keypair.json from Step 4)
ADMIN_PRIVATE_KEY=<your_operator_private_key_u8array_or_b58>

# Keys allowed to mint on the Contra payment channel (comma-separated public keys)
# For testing, use your operator's public key
CONTRA_ADMIN_KEYS=<operator_pubkey>

# Solana Devnet RPC
DEVNET_RPC_URL=https://api.devnet.solana.com

# Yellowstone gRPC (required for real-time indexing)
DEVNET_YELLOWSTONE_ENDPOINT=<your_yellowstone_grpc_endpoint>
INDEXER_YELLOWSTONE_TOKEN=<your_yellowstone_auth_token>

# Optional: Grafana alert webhook (defaults to empty if not set)
# ALERT_WEBHOOK_URL=<your_webhook_url>
```

**Make sure** to update each of these environment variables and ensure there are no duplicate keys before proceeding.

## Step 7: Start All Services

Once your docker build (Step 1) is complete, run: 

```shell
docker compose -f docker-compose.devnet.yml --env-file .env.devnet up -d
```

You should see all services in a healthy/running state:

```shell
[+] Running 21/21a The requested image's platform (linux/amd64) does not match the detected host platfo
 ✔ Network contra_contra-network Created0.0s 
 ✔ Container contra-cadvisor Started2.4s 
 ✔ Container contra-postgres-primary Healthy13.1s                                
 ✔ Container contra-postgres-indexer Healthy14.1s                                
 ✔ Container contra-grafana Started2.5s  ✔ Container contra-prometheus Started2.5s  ✔ Container contra-indexer-solana Started12.2s
 ✔ Container contra-operator-solana Started12.2s
 ✔ Container contra-postgres-replica Started12.7s
 ✔ Container contra-write-node Started2.2s   ✔ Container contra-read-node Started12.4s 
 ✔ Container contra-operator-contra Started11.9s 
 ✔ Container contra-indexer-contra Started12.8s 
 ✔ Container contra-gateway Started12.3s 
```

Check logs if needed:

```shell
# All services
docker compose -f docker-compose.devnet.yml --env-file .env.devnet logs -f


# Specific service
docker compose -f docker-compose.devnet.yml --env-file .env.devnet logs -f indexer-solana
```

For reference, here are the ports and endpoints that are now running:

| Service | Port | Description |
| :---- | :---- | :---- |
| Gateway | `8899` | Main RPC endpoint (routes to read/write nodes) |
| Write Node | `8900` | Handles transaction submissions |
| Read Node | `8901` | Handles read requests (getAccountInfo, etc.) |
| PostgreSQL Primary | `5432` | Contra state database (write) |
| PostgreSQL Replica | `5433` | Contra state database (read) |
| PostgreSQL Indexer | `5434` | Indexer/operator database |
| Admin UI | `5173` | Web interface for instance management |
| Grafana | `37429` | Metrics dashboard (default password: `admin`) |
| Prometheus | `9090` | Metrics collection |
| cAdvisor | `8080` | Container metrics |

## 

## Step 8: Test Deposits and Withdrawals

### Deposit (Solana → Contra)

1. In the Admin UI, scroll down to **User Functions**
2. Enter your whitelisted token that you are holding in the connected wallet
3. Enter an amount and click **"Deposit"** (make sure to include decimals for precision, e.g., 1 USDC should be 1000000)
4. Approve the transaction in your wallet

The indexer will detect the deposit and the operator will mint equivalent tokens on the Contra payment channel.

![Deposit](./assets/deposit-funds.png)

### Verify Deposit on Contra

You can verify your token is on the Contra instance by navigating to **Contra Management** at the top of the screen. Paste the mint’s address and click “Check Balance”. You should see that your tokens have landed on Contra!

### Transfer (Within the Contra Payment Channel)

After your balance has been verified on Contra, you should now have an option to Transfer funds to another user. This is a simple way to demonstrate using the Contra payment channel.

1. **Important**: Since we are working on the Contra payment channel, you must switch your wallet’s RPC before transferring. Change it to **Localnet** or **Custom** (varies by wallet provider) and enter `http://localhost:8899` (the local gateway for your Contra RPC)
2. Enter a user destination address and amount (with decimal precision)
3. Click send and confirm the transaction in your wallet!
4. You can check your Contra balance again and notice that the funds have been debited by your transfer amount.


### Withdraw (Contra → Solana)

1. In the Admin UI, go back to **Escrow Management**
2. Paste the token mint address and enter withdrawal amount
3. **Important**: Before withdrawing, make sure your wallet’s RPC is connected to **Localnet** or **Custom** and enter `http://localhost:8899` (the local gateway for your Contra RPC)
4. Click **"Withdraw"** and approve the transaction
5. (Make sure to switch your wallet back to Devnet when you’re ready to do more devnet activity)

The indexer detects the burn on Contra, builds a Merkle proof, and the operator releases funds from the Solana escrow. You should be able to check your balance in your wallet or on Solana explorer to see the withdrawal.

## Stopping Services

```shell
docker compose -f docker-compose.devnet.yml --env-file .env.devnet down
```

You should see something like this:

```shell
[+] Running 14/14
 ✔ Container contra-indexer-contra    Removed         10.7s 
 ✔ Container contra-gateway           Removed          0.7s 
 ✔ Container contra-operator-contra   Removed         10.7s 
 ✔ Container contra-grafana           Removed          0.5s 
 ✔ Container contra-operator-solana   Removed         10.5s 
 ✔ Container contra-cadvisor          Removed          0.7s 
 ✔ Container contra-indexer-solana    Removed         10.7s 
 ✔ Container contra-prometheus        Removed          0.6s 
 ✔ Container contra-read-node         Removed          0.6s 
 ✔ Container contra-postgres-indexer  Removed          0.8s 
 ✔ Container contra-postgres-replica  Removed          0.9s 
 ✔ Container contra-write-node        Removed          0.6s 
 ✔ Container contra-postgres-primary  Removed          0.5s 
 ✔ Network contra_contra-network      Removed          0.2s
```

To also remove volumes (reset all state):

```shell
docker compose -f docker-compose.devnet.yml --env-file .env.devnet down -v
```

## Troubleshooting

### Services won't start

- Ensure Docker has enough resources allocated (4GB+ RAM recommended)  
- Check that all required environment variables are set  
- Verify your Yellowstone endpoint is accessible and enabled for Devnet

### Transactions failing

- Ensure operator has Devnet SOL for fees  
- Verify the mint is whitelisted on the instance  
- Try using CLI tools in `scripts/devnet/` instead of the Admin UI  
- Check operator logs: `docker compose -f docker-compose.devnet.yml --env-file .env.devnet logs operator-solana`  
  - *Transaction failed: InstructionError(1, Custom(4))* error suggests that the admin environment variable is misconfigured. Check your ENV vars and restart your services. You may need to initialize a new instance/mint afterwards. Or, remove the volumes and start fresh `docker compose -f docker-compose.devnet.yml --env-file .env.devnet down -v`.
- If using the Admin UI, ensure your wallet is on the correct cluster for the correct task (instructions relating to instance management and deposits should use Devnet, and transfers/withdrawals should use your Contra RPC URL (localhost:8899 in our example))

### Indexer not detecting events

- Confirm Yellowstone endpoint and token are correct  
- Ensure environment variables are properly configured  
- For debugging, check if backfill is needed (see config files in `scripts/devnet/config/`)

## Get Help

Contra is still in the early stages of development. If you run into issues or bugs, please [create an issue](https://github.com/solana-foundation/contra/issues) and outline your steps to reproduce it. 

## Configuration Reference

The TOML config files in `scripts/devnet/config/` allow fine-tuning:

| File | Purpose |
| :---- | :---- |
| `indexer-solana.toml` | Solana chain indexer (Yellowstone) |
| `indexer-contra.toml` | Contra payment channel indexer (RPC polling) |
| `operator-solana.toml` | Processes deposits → mints on Contra |
| `operator-contra.toml` | Processes withdrawals → releases on Solana |

**Note:** The TOML files contain placeholder values. When running via Docker Compose, the environment variables from `.env.devnet` override these values at runtime. You do not need to edit the TOML files directly — configure everything through `.env.devnet`.

*Note: for the demo, we have disabled backfills — if your use case requires it, we recommend the `start_slot` be just before the slot you created your instance to avoid unnecessary polling.*

## Learn More

- [Escrow Interaction Guide](./ESCROW_INTERACTION_GUIDE.md) — Programmatic escrow interactions  
- [Withdrawing Guide](./WITHDRAWING_GUIDE.md) — Deep dive on the withdrawal flow