
# Indexer Architecture

## Indexer Components

Monitors Solana Mainnet and the Solana Private Channels payment channel for deposits/withdrawals and writes to database.

### Datasource Strategies

**1. Yellowstone gRPC**

Real-time block streaming via gRPC (requires a gRPC endpoint). Handles both Escrow and Withdraw program types.

**Location**: [`indexer/src/indexer/datasource/yellowstone/`](../indexer/src/indexer/datasource/yellowstone/)


**2. RPC Polling (Mainnet or Solana Private Channels)**

Enumerates the producing slots in each batch with `getBlocks`, then fetches only those blocks in parallel with `getBlock`. Higher latency (~1-5 seconds) but no special infrastructure required.

**Location**: [`indexer/src/indexer/datasource/rpc_polling/`](../indexer/src/indexer/datasource/rpc_polling/)


**3. Vixen**

Alternative datasource using the Vixen parsing framework for instruction decoding.

**Location**: [`indexer/src/datasource/vixen/`](../indexer/src/datasource/vixen/)

### Backfill Strategy

Recovers missed slots on indexer restart or network issues:
1. Read last processed slot from database (`indexer_state` table). That checkpoint is the
   lower bound. A configured `start_slot` only applies to a ledger that has never been
   indexed; one set above an existing checkpoint would skip the slots in between, so the
   indexer refuses to start instead. The same rule covers
   `indexer.rpc_polling.start_slot` when backfill is disabled, where no fill exists to
   recover those slots at all. See
   [`indexer_start_slot_ahead_of_checkpoint.md`](runbooks/indexer_start_slot_ahead_of_checkpoint.md)
2. Query RPC for current slot
3. If gap > threshold, for each batch of slots:
   - Enumerate which slots in the batch produced a block (`getBlocks`)
   - Fetch only those blocks in parallel (configurable batch size)
   - Walk their `parentSlot` links to prove the remaining slots empty; a slot that
     cannot be proven empty aborts the batch rather than being checkpointed past
   - Process blocks in order
   - Update checkpoint per slot via `CheckpointWriter` (driven by `SlotComplete` events)
4. For the Yellowstone datasource, persist a startup anchor before the live stream runs, so a
   durable checkpoint always exists: every connection, the first one included, replays from it up
   to the slot the stream opened at, and withholds live slots rather than advancing the checkpoint
   without one. The anchor is the resolved backfill range's floor, or the current chain tip when
   backfill is disabled. RPC polling has no reconnect repair and writes no anchor; it resumes from
   its configured start slot
5. Switch to real-time mode (Yellowstone or polling)

**Location**: [`indexer/src/indexer/backfill.rs`](../indexer/src/indexer/backfill.rs)

### Transaction Identity & CPI Indexing

Each indexed instruction is keyed on the triple **`(signature, instruction_index, inner_index)`**:

- `instruction_index` — absolute position of the top-level instruction (or, for a CPI, of its top-level ancestor) in the transaction.
- `inner_index` — `NULL` for a top-level instruction; otherwise the instruction's position in the **flattened inner-instruction list** of that top-level ancestor.

**This works at any CPI depth, not just one level.** The validator flattens *every* CPI depth under a top-level instruction into a single inner-instruction list (`meta.innerInstructions[i].instructions`), each entry carrying a `stackHeight`. So a deposit invoked two or more hops deep (`A → B → escrow.Deposit`) is still one entry in that flat list with a unique `inner_index` — `inner_index` is a flat position, **not** a nesting level. Deposit-event scoping likewise keys on `stackHeight` (it walks the contiguous run of deeper entries after the deposit), so it resolves the correct `DepositEvent` regardless of nesting depth.

**Locations**: identity column [`indexer/src/storage/common/models.rs`](../indexer/src/storage/common/models.rs); position capture [`InstructionLocation`/`InnerLocation`](../indexer/src/indexer/datasource/common/types.rs); event scoping `parse_deposit` in [`escrow.rs`](../indexer/src/indexer/datasource/common/parser/escrow.rs).


## Operator Components

Processes pending deposits/withdrawals and executes transactions between Solana Mainnet and the Solana Private Channels payment channel.

### Three-Stage Pipeline

**Location**: [`indexer/src/operator/`](../indexer/src/operator/)

#### 1. Fetcher

Polls database for pending transactions with row-level locking to prevent duplicate processing. Uses PostgreSQL `SELECT FOR UPDATE SKIP LOCKED` to prevent duplicate processing.

**Location**: [`indexer/src/operator/fetcher.rs`](../indexer/src/operator/fetcher.rs)


#### 2. Processor

Validates transactions and builds Solana instructions that are managed by the Solana Private Channels instance's authorized operators/admins. The processor is responsible for three main tasks:
- Processing deposits (Mainnet → Solana Private Channels) - handles building a `MintTo` instruction for the user on the Solana Private Channels payment channel.
- Processing withdrawals (Solana Private Channels → Mainnet) - handles building a `ReleaseFunds` instruction (using the Escrow Program's SMT proof) for the user on Mainnet.
- Rotating the SMT root on the Mainnet escrow instance to prevent double spending of withdrawals.

**Location**: [`indexer/src/operator/processor.rs`](../indexer/src/operator/processor.rs)


#### 3. Sender

Submits transactions to the respective cluster with:
- Exponential backoff retry (configurable max attempts)
- Transaction confirmation polling
- Status updates to database (processing → completed/failed)
- Just-in-time mint initialization (if mint is not yet initialized on the Solana Private Channels payment channel, the Sender will include an `InitializeMint` instruction in the transaction prior to the `MintTo` instruction)

**Location**: [`indexer/src/operator/sender/`](../indexer/src/operator/sender/)

### Additional Components

#### Reconciliation

Runs alongside the three-stage pipeline to detect and resolve discrepancies between on-chain state and the indexer database. Runtime reconciliation checks a single on-chain invariant, `channel_supply <= custody`, over finalized reads and fails closed on a proven insolvency: an insolvency-direction gap exceeding the in-flight envelope for three consecutive finalized ticks trips a durable DB halt flag that freezes both operators' fetchers (plus quarantine + forced-unhealthy + mandatory webhook); recovery is manual per [`docs/runbooks/reconciliation_halt_runbook.md`](runbooks/reconciliation_halt_runbook.md). The custody-vs-ledger comparison now runs only at startup, where it also enforces the same supply invariant before the pipeline boots.

**Location**: [`indexer/src/operator/reconciliation.rs`](../indexer/src/operator/reconciliation.rs), [`indexer/src/indexer/reconciliation.rs`](../indexer/src/indexer/reconciliation.rs)

#### DB Transaction Writer

Handles batched database writes for transaction status updates from the operator pipeline.

**Location**: [`indexer/src/operator/db_transaction_writer.rs`](../indexer/src/operator/db_transaction_writer.rs)

#### Program Type

The indexer uses a `ProgramType` enum (`Escrow` | `Withdraw`) to determine which pipeline branch runs. This is why two parallel instances are deployed: one watching the Escrow program on Mainnet, and one watching the Withdraw program on the Solana Private Channels payment channel.
