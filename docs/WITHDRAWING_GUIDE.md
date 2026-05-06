# Withdrawing Tokens from Solana Private Channels

This guide explains how to withdraw tokens from the Solana Private Channels payment channel back to Solana Mainnet using Sparse Merkle Tree (SMT) proofs.

Want to jump to the code example? [Jump to the TypeScript example](#initiate-a-withdrawal-on-private_channel)

## Overview

Withdrawals move tokens from the Solana Private Channels payment channel to Solana Mainnet through a three-step process:

1. **Burn on Solana Private Channels**: User calls `WithdrawFunds` instruction to burn tokens on the Solana Private Channels payment channel
2. **Backend Processing**: Indexer detects burn event, builds SMT proof, and submits to Mainnet
3. **Release on Mainnet**: Operator calls `ReleaseFunds` instruction with cryptographic proof to unlock escrowed tokens

The [Indexer/Operator](../indexer/src/operator/) handles steps 2 and 3 automatically. This guide explains how the withdraw process works and how to manually initiate a withdrawal on Solana Private Channels.

## Understanding the Sparse Merkle Tree (SMT)

Solana Private Channels uses a **Sparse Merkle Tree** to prevent double-spending of withdrawals. Each withdrawal is assigned a unique `transaction_nonce` that gets recorded in the tree. The mainnet escrow program validates each withdrawal's nonce and tree index to prevent double processing of the same withdrawal.

### Tree Structure
- **Height**: 16 levels
- **Max Leaves**: 65,536 (2^16) transaction nonces per tree
- **Leaf Value**:
  - Empty leaf: `[0u8; 32]` (nonce not present)
  - Non-empty leaf: `SHA256([1u8; 32])` (nonce present)
- **Root Hash**: 32-byte commitment to all recorded nonces

```
                        Root Hash (32 bytes)
                       /                    \
                 Hash(L, R)                Hash(L, R)
                /         \                /         \
          Hash(L, R)   Hash(L, R)   Hash(L, R)   Hash(L, R)
          /      \      /      \      /      \      /      \
        ...    ...    ...    ...    ...    ...    ...    ...
       /  \   /  \   /  \   /  \   /  \   /  \   /  \   /  \
   Leaf0  1  2   3  4   5  6   7  8   9  10 11 12 13 14 15 ...
   (nonces recorded as leaf positions using their value modulo 65536)
```

### Why SMT?

Traditional Merkle trees require storing all intermediate nodes. SMTs are "sparse" because:
- Most leaves are empty (default `[0u8; 32]`)
- Only compute/store paths for non-empty leaves
- Efficient for tracking which nonces have been used (prevents replay attacks)
- The Mainnet escrow program withdraw instruction verifies that the nonce is _not_ already in the current tree by providing an exclusion proof AND that the nonce is _in_ the new tree by providing an inclusion proof.

### The Rotating Tree Index System

Solana Private Channels uses a **rotating tree index** mechanism to handle unlimited withdrawals while keeping the SMT bounded and limited in size. This helps minimize account size, transaction size, and processing costs/compute.

Each Solana Private Channels instance has its own tree with two important fields stored in the `Instance` state:
- `withdrawal_transactions_root`: The root hash of the tree
- `current_tree_index`: The index of the current tree

```rust
pub struct Instance {
    pub withdrawal_transactions_root: [u8; 32],
    pub current_tree_index: u64,
    // ... other fields
}
```

The `current_tree_index` determines which "generation" of the tree a nonce belongs to:

```rust
let expected_tree_index = transaction_nonce.checked_div(MAX_TREE_LEAVES as u64)
            .ok_or(ProgramError::ArithmeticOverflow)?;
```

The `expected_tree_index` is validated against the instance's `current_tree_index` to prevent double processing of the same withdrawal.

The `leaf_position` determines the position of the leaf in the tree:

```rust
let leaf_position = transaction_nonce as usize % MAX_TREE_LEAVES;
```

**Examples:**
| Transaction Nonce | Tree Index | Position in Tree |
|------------------|------------|------------------|
| 0 | 0 | Leaf 0 |
| 1 | 0 | Leaf 1 |
| 65,535 | 0 | Leaf 65,535 |
| 65,536 | 1 | Leaf 0 (new tree) |
| 65,537 | 1 | Leaf 1 (new tree) |
| 131,071 | 1 | Leaf 65,535 (new tree) |
| 131,072 | 2 | Leaf 0 (new tree) |

### Tree Lifecycle

**Initial State:**
```rust
Instance {
    withdrawal_transactions_root: EMPTY_TREE_ROOT, // All zeros
    current_tree_index: 0,
}
```

**After 65,536 Withdrawals (Tree Full):**

The operator calls `ResetSmtRoot` to rotate to the next tree:

```rust
// Operator-only instruction (automatically handled)
ResetSmtRoot {
    withdrawal_transactions_root: EMPTY_TREE_ROOT, // Reset to empty
    current_tree_index: 1,                         // Increment to next generation
}
```

**Key Properties:**
- **No replay attacks**: Old nonces (tree_index 0) cannot be used in new tree (tree_index 1)
- **Unbounded withdrawals**: Rotate trees indefinitely (tree_index 0→1→2→...→2^64)
- **Constant verification cost**: Always verify against 16-level tree (O(log n) complexity)

### Visual Example

```
Tree Index 0 (nonces 0-65,535)              Tree Index 1 (nonces 65,536-131,071)
┌────────────────────────────┐             ┌────────────────────────────┐
│ Root: 0x8fe6...            │             │  Root: 0x8fe6... (reset)   │
│ Nonces Used: 65,536/65,536 │   Rotate    │  Nonces Used: 0/65,536     │
│ Status: FULL               │   ──────>   │  Status: ACTIVE            │
└────────────────────────────┘             └────────────────────────────┘
         (Tree exhausted)                          (Fresh tree)
```


## Initiate a Withdrawal on Solana Private Channels

Users initiate withdrawals by burning tokens on the Solana Private Channels payment channel using the Withdrawal Program. This will burn tokens from Solana Private Channels. The Solana Private Channels Indexer/Operator will monitor for these transactions and then process the `ReleaseFunds` instruction on Mainnet.

### TypeScript Example

```typescript
import {
  getWithdrawFundsInstructionAsync,
  PRIVATE_CHANNEL_WITHDRAW_PROGRAM_PROGRAM_ADDRESS
} from 'private-channel-withdraw-program';
import { address, generateKeyPairSigner, none } from '@solana/kit';

const user = await generateKeyPairSigner();
const withdrawAmount = 1_000_000n; // 1 USDC (6 decimals)
const USDC_MINT = address('EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v');

// Optional: Specify destination address on Mainnet (defaults to user if null)
const destinationOnMainnet = address('DestinationAddressOnMainnet...');

// Build withdraw instruction
const withdrawIx = await getWithdrawFundsInstructionAsync({
  user,
  mint: USDC_MINT,
  amount: withdrawAmount,
  destination: none(), // Optionally pass a destination address on Mainnet
});

// Send to Solana Private Channels RPC
const private_channelRpc = createSolanaRpc(createDefaultRpcTransport({ url: 'https://private-channel-rpc.example.com' }));
// ... sign and send transaction
```

**Key Points:**
- **Permissionless**: Any user can burn their tokens on Solana Private Channels
- **Destination Field**:
  - If `null`: Tokens released to `user` address on Mainnet
  - If specified: Tokens released to `destination` address on Mainnet (associated token account must already exist for this user's address on Mainnet)
- Executing the `WithdrawFunds` instruction will burn tokens from the Solana Private Channels payment channel immediately.

### Related Documentation
- [Escrow Interaction Guide](ESCROW_INTERACTION_GUIDE.md)
- [Architecture Overview](ARCHITECTURE.md)
- [Escrow Program Technical Reference](ESCROW_PROGRAM.md)
- [Withdrawal Program Technical Reference](WITHDRAW_PROGRAM.md)
- [Indexer Architecture](INDEXER.md)
