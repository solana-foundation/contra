# Contra Admin UI

A modern web-based admin interface for managing Contra instances on Solana. **Fully migrated to @solana/kit v2** with zero deprecated dependencies!

## ✅ Full Migration Complete

This admin UI has been **completely migrated** from the deprecated `@solana/web3.js` v1 to the modern **@solana/kit** SDK.

### What Was Removed
- ❌ `@solana/web3.js` v1.x - **COMPLETELY REMOVED**
- ❌ `@solana/wallet-adapter-*` - **COMPLETELY REMOVED**
- ❌ `@coral-xyz/anchor` - **COMPLETELY REMOVED**

### What's Now Used
- ✅ `@solana/rpc` - Modern RPC client
- ✅ `@solana/rpc-subscriptions` - WebSocket subscriptions
- ✅ `@solana/addresses` - Type-safe address handling
- ✅ `@solana/transactions` - Functional transaction building
- ✅ `@wallet-standard/react` - Modern wallet connections
- ✅ Codama-generated clients (native @solana/kit types)

### Bundle Size Improvement

**Before Migration:**
- Bundle: 195.43 KB gzipped

**After Migration:**
- Bundle: **78.96 KB gzipped**
- **Improvement: 60% smaller!**

## Features

### Instance Management
- ✅ **Load and view instances** - Fully working with new RPC API
- ✅ **Display instance data** - Admin, seed, withdrawal root, tree index
- ⏳ Create new instances - UI ready, transaction signing TODO

### Status Checking
- ✅ **Check mint status** - Fully working
- ✅ **Check operator status** - Fully working

### Admin Functions (UI ready, transactions TODO)
- Mint Management: Allow/block token mints
- Operator Management: Add/remove operators
- Admin Transfer: Transfer admin rights
- Create Mint: Generate new SPL tokens

### Operator Functions (UI ready, transactions TODO)
- Release Funds: Release escrowed funds with SMT proofs
- Reset SMT Root: Reset the Sparse Merkle Tree

### User Functions (UI ready, transactions TODO)
- Deposit: Deposit tokens to escrow
- Withdraw: Withdraw tokens from account

## Getting Started

### Prerequisites
- Node.js 18+
- pnpm (required - `npm install -g pnpm`)
- A Solana wallet browser extension (Phantom, Solflare)

### Installation

```bash
cd admin-ui
pnpm install
```

### Development

```bash
pnpm run dev
```

App runs at `http://localhost:5173`

### Build

```bash
pnpm run build
```

Output: `dist/` directory with 78.96 KB gzipped bundle

## Configuration

### Network Selection

Use the dropdown in the UI to switch between:
- Devnet (default)
- Testnet
- Mainnet
- Localnet

### Custom RPC Endpoint

Create `.env`:
```
VITE_RPC_ENDPOINT=https://your-custom-rpc.com
```

## Architecture

### Modern Provider Stack

```typescript
// ClusterContext - Network management
<ClusterProvider>
  // SolanaProvider - RPC + Wallet Standard
  <SolanaProvider endpoint={endpoint} wsEndpoint={wsEndpoint}>
    <App />
  </SolanaProvider>
</ClusterProvider>
```

### Key Components

**SolanaProvider** (`src/providers/SolanaProvider.tsx`)
- Creates RPC connections with `createSolanaRpc()`
- Manages wallet state via `useWallets()` from Wallet Standard
- Provides `useSolana()` and `useWalletAddress()` hooks

**ConnectWalletButton** (`src/components/ConnectWalletButton.tsx`)
- Discovers installed wallets via Wallet Standard
- No wallet-specific adapters needed
- Works with any standard-compliant wallet

**InstanceManager** (`src/components/InstanceManager.tsx`)
- Uses `rpc.getAccountInfo()` to fetch on-chain data
- Decodes using codama-generated `decodeInstance()`
- Browser-native base64 decoding (no Node.js Buffer)

**StatusChecker** (`src/components/StatusChecker.tsx`)
- Verifies mint allowances
- Checks operator authorization
- Uses new RPC API for account lookups

## Implementation Status

### ✅ Fully Working

1. **Wallet Connection**
   - Wallet Standard protocol
   - Auto-discovers installed wallets
   - Connect/disconnect functionality

2. **RPC Communication**
   - `createSolanaRpc()` for queries
   - `createSolanaRpcSubscriptions()` for WebSocket
   - Network switching

3. **Data Fetching**
   - Load instance data
   - Check mint/operator status
   - Decode account data

### ⏳ Transaction Signing (TODO)

The UI is complete, but transaction submission needs implementation using:

```typescript
import { pipe } from '@solana/functional';
import {
  createTransactionMessage,
  setTransactionMessageFeePayer,
  appendTransactionMessageInstructions,
  setTransactionMessageLifetimeUsingBlockhash
} from '@solana/transaction-messages';
import { signAndSendTransactionMessageWithSigners } from '@solana/signers';

// Example pattern for implementing transactions
const { value: latestBlockhash } = await rpc.getLatestBlockhash().send();

const transactionMessage = pipe(
  createTransactionMessage({ version: 0 }),
  tx => setTransactionMessageFeePayer(walletAddress, tx),
  tx => setTransactionMessageLifetimeUsingBlockhash(latestBlockhash, tx),
  tx => appendTransactionMessageInstructions([instruction], tx)
);

// Sign and send via Wallet Standard
const signedTx = await wallet.features['solana:signAndSendTransaction'].signAndSendTransaction({
  transaction: transactionMessage
});
```

## Migration Details

### No More Buffer!

The app uses **browser-native APIs** only:
```typescript
// OLD (Node.js Buffer)
const data = Buffer.from(base64, 'base64');

// NEW (Browser native)
const binaryString = atob(base64);
const data = new Uint8Array(binaryString.length);
for (let i = 0; i < binaryString.length; i++) {
  data[i] = binaryString.charCodeAt(i);
}
```

### No More Classes!

```typescript
// OLD
new PublicKey(address)
new Connection(endpoint)
new Transaction()

// NEW
address(addressString)
createSolanaRpc(endpoint)
createTransactionMessage({ version: 0 })
```

### No More Wallet Adapters!

```typescript
// OLD
import { useWallet } from '@solana/wallet-adapter-react';
const { publicKey, sendTransaction } = useWallet();

// NEW
import { useSolana, useWalletAddress } from './providers/SolanaProvider';
const { wallets, connect } = useSolana();
const walletAddress = useWalletAddress();
```

## Dependencies

```json
{
  "@solana/rpc": "^2.0.3",
  "@solana/rpc-subscriptions": "^2.0.3",
  "@solana/addresses": "^2.0.3",
  "@solana/transactions": "^2.0.3",
  "@solana/transaction-messages": "^2.0.3",
  "@solana/signers": "^2.0.3",
  "@wallet-standard/react": "^1.0.1",
  "@wallet-standard/core": "^1.0.1"
}
```

**Zero deprecated packages!**

## Troubleshooting

### "No wallets found"
- Install Phantom or Solflare browser extension
- Refresh the page after installation
- Ensure wallet extension is enabled

### "Buffer is not defined"
- This has been fixed! Using browser-native `atob()` now
- No Node.js polyfills needed

### Transaction Errors
- Transaction signing is marked as TODO
- Use CLI tools for transactions until implemented
- Implementation requires Wallet Standard transaction signing

## Next Steps

To implement transaction signing:

1. Review Solana's official examples: https://solana.com/docs/frontend/kit
2. Implement `signAndSendTransaction` via Wallet Standard
3. Use functional `pipe()` composition for transaction building
4. Add error handling and confirmation tracking

## Contributing

Transaction implementation is the main TODO. The infrastructure is complete:
- ✅ Modern SDK integration
- ✅ Wallet Standard connection
- ✅ RPC queries working
- ✅ Data fetching and decoding
- ⏳ Transaction signing

PRs welcome for transaction implementation!

## Resources

- [@solana/kit GitHub](https://github.com/anza-xyz/kit)
- [Wallet Standard](https://github.com/wallet-standard/wallet-standard)
- [Migration Guide](https://blog.triton.one/intro-to-the-new-solana-kit-formerly-web3-js-2/)

---

**Built with the modern Solana SDK - No deprecated dependencies!** 🚀

**Bundle: 78.96 KB gzipped (60% smaller than before!)**
