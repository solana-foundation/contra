# Contra K6 Load Tests

K6-based load testing for the Contra Solana sequencer using SPL token transfers.

## Setup

1. **Install K6**:
   ```bash
   # macOS
   brew install k6

   # Ubuntu/Linux
   snap install k6
   ```

2. **Configure Environments**:
   ```bash
   # Copy example to create environment configs
   cp .env.example .env.local
   cp .env.example .env.cloud

   # Edit each file:
   # - .env.local: RPC_URL=http://localhost:8899
   # - .env.cloud: RPC_URL=http://write.onlyoncontra.xyz
   ```

## Usage

```bash
# Run against LOCAL
./run.sh local

# Run against CLOUD (default)
./run.sh
./run.sh cloud
```

## How It Works

The test sends pre-generated SPL token transfer transactions to the RPC endpoint. This matches the approach used in `core/src/bin/load_test.rs`.

### Generating Test Transactions

To create new test transactions:
```bash
node create-transfer-tx.js  # Creates SPL token transfers
```

## Test Configuration

In `src/load-test.ts`:
- **VUs**: 10 concurrent users
- **Duration**: 30 seconds
- **Success threshold**: >95% success rate
- **Performance threshold**: p95 < 500ms

## Files

- `src/load-test.ts` - Main k6 test
- `create-transfer-tx.js` - Generate valid SPL transactions
- `.env.local` / `.env.cloud` - Environment configs
- `run.sh` - Test runner script

## Metrics

- **send_duration**: Transaction send time
- **send_success**: Success rate
- **http_req_duration**: Overall HTTP request time

## Notes

- The RPC only accepts SPL token and ATA transactions
- Transactions must be properly signed and serialized
- The test uses the same transaction format as the Rust load test