# Contra Load Test Demo App

A visual web interface for load testing the Contra Solana deployment. This app replicates the functionality of the core `load_test.rs` with an intuitive UI for monitoring transactions and wallet balances in real-time.

## Features

- **Interactive Load Testing**: Configure and run load tests with customizable parameters
- **Real-time Visualization**:
  - Live wallet balance updates for senders and receivers
  - Animated transaction flow visualization
  - Success/failure statistics with throughput metrics
- **Comprehensive Monitoring**:
  - Transaction confirmation tracking with polling indicators
  - Average latency measurements
  - Success rate visualization
  - Throughput calculations (transactions per second)

## Setup

### Prerequisites

- Node.js 18+
- pnpm package manager

### Installation

```bash
# Install dependencies
pnpm install
```

### Configuration

1. Copy the example environment file:
```bash
cp .env.example .env
```

2. Configure the environment variables in `.env`:
- `VITE_WRITE_URL`: URL for write operations (default: https://write.onlyoncontra.xyz)
- `VITE_READ_URL`: URL for read operations (default: https://read.onlyoncontra.xyz)
- `VITE_ADMIN_KEYPAIR`: Admin keypair array for funding test wallets

## Development

```bash
# Start the development server
pnpm dev
```

The app will be available at http://localhost:5173

## Production Build

```bash
# Build for production
pnpm build

# Preview production build
pnpm preview
```

## Usage

1. **Configure Test Parameters**:
   - **Users**: Number of concurrent users (1-20)
   - **Duration**: Test duration in seconds (5-300)
   - **Request Delay**: Delay between requests in milliseconds (10-5000)

2. **Start the Test**:
   - Click "Start Test" to begin the load test
   - The app will:
     - Create sender and receiver wallets
     - Fund sender wallets from the admin account
     - Execute transfers between random sender-receiver pairs
     - Poll for transaction confirmations
     - Update wallet balances in real-time

3. **Monitor Results**:
   - Watch the transaction flow with animated transfers
   - Track success/failure rates in the statistics panel
   - Monitor average latency and poll counts
   - View real-time throughput metrics

## Architecture

The app uses:
- **Vite** for fast development and optimized builds
- **React** with TypeScript for type-safe component development
- **@solana/web3.js** for blockchain interactions
- **Framer Motion** for smooth animations
- **Lucide React** for icons

## Key Components

- **LoadTestController**: Configuration panel for test parameters
- **WalletVisualizer**: Displays sender/receiver wallets with live balances
- **TransactionFlow**: Animated visualization of ongoing transactions
- **Statistics**: Real-time metrics and success rate tracking
- **useLoadTest**: Core hook managing the test lifecycle and blockchain interactions

## How It Works

1. **Wallet Creation**: Creates specified number of sender and receiver wallets
2. **Funding**: Transfers SOL from admin account to sender wallets
3. **Transaction Loop**: Continuously sends transactions between random pairs
4. **Confirmation Polling**: Monitors transaction status until confirmed/failed
5. **Balance Updates**: Refreshes wallet balances after each confirmation
6. **Statistics Tracking**: Calculates throughput, latency, and success rates

## Troubleshooting

- **Connection Issues**: Verify the write/read URLs are accessible
- **Funding Failures**: Ensure the admin keypair has sufficient SOL balance
- **Transaction Failures**: Check network congestion and RPC limits