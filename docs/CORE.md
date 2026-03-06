
# Contra Core

Contra processes transactions through five sequential stages, each optimized for a specific concern.

```
Transaction → [1:Dedup] → [2:SigVerify] → [3:Sequencer] → [4:Executor] → [5:Settler] → Database
```

### Stage 1: Dedup

Filter duplicate transactions before expensive signature verification:
- Validates that a transaction's blockhash is in the set of live blockhashes (populated from settled blocks). Transactions referencing unknown or expired blockhashes are rejected.
- Maintains a cache of recently seen transaction signatures keyed by blockhash.
- Filters duplicate submissions before expensive signature verification.
- Invalidates blockhashes after a configurable duration, e.g., 15 seconds (150 blockhashes × 100ms block time).

**Location**: [`core/src/stages/dedup.rs`](../core/src/stages/dedup.rs)

**Code Snippet**:
```rust
// Check for duplicate
let is_duplicate = dedup_cache // HashMap<Hash, HashSet<Signature>>
    .get(&blockhash)
    .map(|sigs| sigs.contains(&signature))
    .unwrap_or(false);

if is_duplicate {
    continue; // Drop duplicate
}

// Add to cache
dedup_cache
    .entry(blockhash)
    .or_default()
    .insert(signature);
```

### Stage 2: SigVerify

Parallelizes Ed25519 signature verification across configurable workers. Each worker independently validates transaction signatures before forwarding to sequencing. Invalid signatures are dropped with error logging.

**Location**: [`core/src/stages/sigverify.rs`](../core/src/stages/sigverify.rs)

### Stage 3: Sequencer

Builds dependency directed acyclic graph (DAG) and produces conflict-free transaction batches:
- Analyzes each transaction's read/write account set to form a DAG.
- Uses a greedy scheduler to produce conflict-free batches (max 64 transactions).
- Transactions touching overlapping writable accounts are placed in separate batches to enable parallel execution.
- Emits batches to the executor.

**Location**: [`core/src/stages/sequencer.rs`](../core/src/stages/sequencer.rs), [`core/src/scheduler/dag.rs`](../core/src/scheduler/dag.rs)

1. **Dependency Analysis**:
   - Read-Read: No conflict (parallel execution allowed)
   - Read-Write: Conflict (must serialize)
   - Write-Write: Conflict (must serialize)

2. **Batch Formation**:
   - Start with empty batch
   - For each transaction in dependency order:
     - If no conflict with current batch → add to batch
     - If conflict → start new batch
   - Emit batches to executor

### Stage 4: Executor

Execute transaction batches through the SVM with custom execution modes.

**Location**: [`core/src/stages/execution.rs`](../core/src/stages/execution.rs), [`core/src/vm/`](../core/src/vm/)


**Execution Modes**:

#### AdminVM

Privileged execution for token mint operations (bypasses BPF execution). This enables consistent mint addresses across Mainnet and the Contra payment channel. This is achieved by intercepting `InitializeMint` instructions and synthesizing mint accounts without executing BPF code.

**Location**: [`core/src/vm/admin.rs`](../core/src/vm/admin.rs)
**Security**: Transactions are gated by admin key validation in the SigVerify stage (`CONTRA_ADMIN_KEYS`). Only transactions signed by an admin key are routed to AdminVM for execution.

#### GaslessCallback

GaslessCallback intercepts SVM account lookups to synthesize fee payer accounts on-demand (fixed lamports, owned by system program):

```rust
fn get_account_shared_data(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
    if let Some(account) = self.bob.get_account_shared_data(pubkey) {
        return Some(account);
    } else if self.fee_payers.contains(pubkey) {
        // Synthesize fee payer with minimal lamports
        return Some(AccountSharedData::new(
            DEFAULT_FEE_PAYER_LAMPORTS,
            0,
            &solana_sdk_ids::system_program::ID,
        ));
    }
    None
}
```

This eliminates the operational overhead of funding user accounts for off-chain execution and results in zero gas fees for all user transactions.

**Location**: [`core/src/vm/gasless_callback.rs`](../core/src/vm/gasless_callback.rs)

#### GaslessRentCollector

Intercepts rent collection to prevent the runtime from debiting lamports from synthesized fee payer accounts. Works alongside GaslessCallback to maintain the zero-fee model.

**Location**: [`core/src/vm/gasless_rent_collector.rs`](../core/src/vm/gasless_rent_collector.rs)


### Stage 5: Settler

Batches execution results every 100ms (configurable) and commits to your configured database (e.g., PostgreSQL, Redis). The settler writes:
- Modified accounts
- Transaction records
- Block metadata (slot, blockhash, timestamp)

Finally, the settler notifies the executor's in-memory cache (BOB) of settled accounts, completing the feedback loop.

**Location**: [`core/src/stages/settle.rs`](../core/src/stages/settle.rs)


## Supported Programs

Contra restricts which programs can execute in the payment channel. Transactions referencing unsupported programs are rejected at the RPC layer.

| Program | Status | Notes |
|---------|--------|-------|
| **SPL Token** | Supported | Full support including Token-2022 |
| **SPL Associated Token Account** | Supported | ATA creation and lookup |
| **SPL Memo** | Supported | Memo attachments |
| **System Program** | Supported | Native transfers and account creation |
| **Contra Withdraw Program** | Supported | Token burns for withdrawal flow |

**Source**: [`core/src/rpc/send_transaction_impl.rs`](../core/src/rpc/send_transaction_impl.rs)

### AdminVM Program Support

The AdminVM (used for operator mint operations) only supports SPL Token `InitializeMint`. All other instruction types are rejected.

**Source**: [`core/src/vm/admin.rs`](../core/src/vm/admin.rs)

## Limitations

### No Custom Program Deployment

Contra does not support deploying arbitrary BPF programs. The supported program set is fixed at compile time. The instruction allowlist is currently hardcoded to SPL Token instructions.

### No Precompiles

Solana precompile programs (Ed25519, Secp256k1, Secp256r1) are not available. Transactions that reference precompile addresses will fail.

### Hardcoded Constraints

| Constraint | Value | Source |
|------------|-------|--------|
| Max transaction size | 1,232 bytes | Solana's `PACKET_DATA_SIZE` |
| Max transactions per batch | 64 (configurable) | `CONTRA_MAX_TX_PER_BATCH` |
| Max loaded accounts data | 64 MB | [`core/src/processor.rs`](../core/src/processor.rs) |
| Max signatures per `getSignatureStatuses` | 256 | [`core/src/rpc/constants.rs`](../core/src/rpc/constants.rs) |
| Max slot range for `getBlocks` | 500,000 | [`core/src/rpc/constants.rs`](../core/src/rpc/constants.rs) |
| Max RPC response size | 10 MB | [`core/src/rpc/constants.rs`](../core/src/rpc/constants.rs) |
| Gateway max request body | 64 KB | [`gateway/src/lib.rs`](../gateway/src/lib.rs) |

### No Fork Choice

Contra does not implement slots or forks. The fork graph is stubbed — all blocks are final on write. There is no rollback mechanism.