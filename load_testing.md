# Contra Load Testing — Research & Plan

## Table of Contents
1. [Current State Analysis](#1-current-state-analysis)
2. [Gaps & Improvement Potential](#2-gaps--improvement-potential)
3. [Best Practices](#3-best-practices)
4. [Load Test Plan: Contra Transfers](#4-load-test-plan-contra-transfers)
5. [Load Test Plan: Deposit to Escrow](#5-load-test-plan-deposit-to-escrow)
6. [Load Test Plan: Withdrawal from Escrow](#6-load-test-plan-withdrawal-from-escrow)
7. [Shared Infrastructure Improvements](#7-shared-infrastructure-improvements)
8. [Implementation Priority](#8-implementation-priority)

---

## 1. Current State Analysis

### 1.1 Existing Test Scripts

The project has a minimal k6 setup under `k6/` with **two test scripts**:

#### `k6/src/send-transaction.ts` — Basic Load Test
- **VUs**: 10 concurrent
- **Duration**: 30 seconds
- **Protocol**: HTTP POST with JSON-RPC 2.0 (`sendTransaction`)
- **Thresholds**: `send_duration` p95 < 500ms, `send_success` rate > 95%
- **Transaction data**: Single hardcoded base64-encoded SPL token transfer
- **Validation**: Checks HTTP 200 + response signature matches expected value
- **HTTP timeout**: 2 seconds

#### `k6/src/max-throughput.ts` — Stress/Throughput Test
- **VUs**: Ramps from 100 → 1000 (10 stages × 5s each, MULTIPLIER=10)
- **Duration**: ~55 seconds total
- **Thresholds**: `send_duration` p95 < 1000ms, `send_success` rate > 90%, `http_req_duration` p95 < 2000ms
- **Transaction data**: Same single hardcoded transaction as above
- **HTTP timeout**: 5 seconds (increased for heavy load)
- **Connection reuse**: Enabled (`noConnectionReuse: false`)
- **Batching**: `batch: 10`, `batchPerHost: 10`
- **Backoff**: Optional sleep(0.1s) on connection errors via `K6_SLEEP` env var
- **Extra metric**: `requests_per_second` (Counter)

### 1.2 Build & Execution Infrastructure

| Component | Details |
|-----------|---------|
| Language | TypeScript compiled via webpack |
| Build | `npm run build` → webpack bundles to `dist/` |
| k6 Types | `@types/k6@0.52.0` |
| Dependencies | `@solana/web3.js`, `@solana/spl-token`, `bs58`, `dotenv`, `express` |
| Runner: basic | `run.sh [local\|cloud]` — npm-based, supports k6 local + k6 Cloud |
| Runner: stress | `run-max-throughput.sh [local\|cloud]` — pnpm-based, supports k6 local + k6 Cloud |
| Local target | `http://localhost:8899` (direct write-node RPC) |
| Cloud target | `https://write.onlyoncontra.xyz` |
| Env config | `RPC_URL` environment variable |

### 1.3 Custom Metrics

| Metric | Type | Used In | Description |
|--------|------|---------|-------------|
| `send_duration` | Trend | Both scripts | Time from HTTP POST to response |
| `send_success` | Rate | Both scripts | Fraction with HTTP 200 + valid signature |
| `requests_per_second` | Counter | max-throughput only | Total requests (for RPS calculation) |

### 1.4 Monitoring Stack (Already in Place)

The project has a comprehensive monitoring stack that load tests should integrate with:

**Prometheus** (port 9090):
- Scrapes: indexer (9100), operator (9100), gateway (9101), cAdvisor, blackbox exporter
- 28+ custom application metrics across services

**Grafana** (port 37429) with 4 dashboards:
- `contra-indexer.json` — Slot processing, transactions saved, lag, errors
- `contra-node-metrics.json` — Pipeline stage metrics (dedup, sigverify, sequencer, executor, settler)
- `contra-operator.json` — Transaction processing, RPC send duration, feepayer balance
- `contra-rpc.json` — RPC health and request metrics

**Alerts**:
- Indexer lag > 10 seconds behind chain tip (warning)
- Service down — scrape target unreachable or health probe failed (critical)
- Feepayer balance < 0.5 SOL (critical)

**Key Prometheus metrics for load test correlation**:

| Service | Metric | Labels |
|---------|--------|--------|
| Gateway | `contra_gateway_requests_total` | method, target, status |
| Gateway | `contra_gateway_request_duration_seconds` | method, target |
| Gateway | `contra_gateway_errors_total` | error_type |
| Operator | `contra_operator_transactions_fetched_total` | program_type |
| Operator | `contra_operator_rpc_send_duration_seconds` | program_type, result |
| Operator | `contra_operator_backlog_depth` | program_type |
| Operator | `contra_operator_transaction_errors_total` | program_type, reason |
| Operator | `contra_operator_mints_sent_total` | program_type |
| Operator | `contra_feepayer_balance_lamports` | program_type |
| Indexer | `contra_indexer_slots_processed_total` | program_type |
| Indexer | `contra_indexer_slot_processing_duration_seconds` | program_type |
| Indexer | `contra_indexer_chain_tip_slot` | program_type |
| Indexer | `contra_indexer_backfill_slots_remaining` | program_type |

### 1.5 Related: Activity Generator

The Rust binary at `core/src/bin/activity.rs` generates continuous traffic (creates users, SPL transfers, mints, withdrawals) against a running Contra deployment. It runs as a Docker Compose service. This is **not a load test** (no thresholds, no reporting, no controllable load profiles) but serves as a useful reference for transaction construction and E2E flows.

---

## 2. Gaps & Improvement Potential

### 2.1 Critical Gaps

| # | Gap | Impact | Priority |
|---|-----|--------|----------|
| 1 | **Single hardcoded transaction defeats dedup stage** | The pipeline's dedup stage filters duplicate transactions. All VUs after the first send the same signature, so the test measures error handling, not throughput. All current load numbers are invalid. | P0 |
| 2 | **No transaction generator** | Cannot produce unique valid transactions at scale. README references `create-transfer-tx.js` which does not exist. | P0 |
| 3 | **No deposit load test** | The deposit flow (Solana → Indexer → Operator → Contra mint) has zero load coverage. | P1 |
| 4 | **No withdrawal load test** | The withdrawal flow (Contra burn → Operator → SMT proof → Solana release) has zero load coverage. | P1 |
| 5 | **No gateway load testing** | Both scripts target the write-node directly (port 8899), bypassing the gateway routing layer (port 8898). Gateway routing overhead, CORS handling, and connection management are untested. | P1 |
| 6 | **No end-to-end verification** | Tests only check HTTP 200 + expected signature. They do not verify that transactions were settled, account balances changed, or the pipeline completed. | P2 |

### 2.2 Infrastructure Gaps

| # | Gap | Impact | Priority |
|---|-----|--------|----------|
| 7 | **No CI/CD integration** | No k6 step in GitHub Actions (`.github/workflows/rust.yml`). Performance regressions go undetected between releases. | P2 |
| 8 | **No Prometheus Remote Write** | k6 has built-in Prometheus output, but it's not configured. Load test metrics cannot be correlated with application metrics in Grafana. | P2 |
| 9 | **No Docker integration** | k6 is not in `docker-compose.yml`. Running against the local stack requires manual k6 installation. | P3 |
| 10 | **No shared utilities** | Each script duplicates constants, HTTP patterns, and metric definitions. | P3 |
| 11 | **Stale README** | References nonexistent files (`create-transfer-tx.js`, `src/load-test.ts`, `.env.local`, `.env.cloud`). | P3 |

### 2.3 Test Design Gaps

| # | Gap | Details |
|---|-----|---------|
| 12 | **Uses simple `vus`/`duration` instead of `scenarios`** | k6's `scenarios` API provides fine-grained control via executors (`constant-arrival-rate`, `ramping-arrival-rate`, `per-VU-iterations`). Current config uses the basic `vus`+`duration` or `stages` approach. |
| 13 | **No test profiles** | No distinction between smoke, load, stress, soak, and spike tests. Both scripts are always-on with fixed configurations. |
| 14 | **No `handleSummary()` for CI** | No machine-readable JSON output for automated pass/fail in pipelines. |
| 15 | **Signature validation too strict** | Tests check `body.result === expectedSignature` — but with unique transactions, each will have a different signature. Validation should check for any valid base58 signature. |

---

## 3. Best Practices

The following k6 best practices should be applied across all test scripts:

1. **Use `SharedArray`** for pre-generated transaction data to avoid per-VU memory duplication.
2. **Use `scenarios`** with explicit executors (`constant-arrival-rate`, `ramping-arrival-rate`) instead of simple `vus`/`duration` for precise load control.
3. **Separate test profiles**: smoke (quick CI gate), load (expected traffic), stress (find breaking point), soak (long-duration stability), spike (sudden burst).
4. **Tag everything**: Environment, test type, and scenario should be tags on all requests for dashboard filtering.
5. **Use `group()`** to organize related checks within a scenario.
6. **Set `discardResponseBodies: true`** when response body content is not needed, to reduce memory.
7. **Measure from the client**: Use custom Trend metrics for business-level latency (not just `http_req_duration`).
8. **Assert with `check()` and fail with `abortOnFail` thresholds** for critical SLOs.
9. **Use `handleSummary()`** to produce machine-readable JSON output for CI consumption.
10. **Integrate with Prometheus** via `--out experimental-prometheus-rw` to correlate load test metrics with application metrics in Grafana.

---

## 4. Load Test Plan: Contra Transfers

### 4.1 Objective

Validate that the Contra 5-stage pipeline (Dedup → SigVerify → Sequencer → Executor → Settler) meets its TPS and latency targets under realistic load. The system targets ~4,000 TPS with ~100ms settlement time.

### 4.2 Test Architecture

```
k6 VUs  ──HTTP POST──>  Gateway (:8898)  ──route──>  write-node (:8900)
                         JSON-RPC 2.0                  |
                         sendTransaction               v
                                                  5-stage pipeline
                                                  dedup -> sigverify(4w) -> sequencer(batch 64) -> executor -> settler(100ms)
```

**Target endpoint**: Gateway at port 8898 (tests full path including routing). Optionally target write-node directly at port 8900 for isolated pipeline benchmarking.

**RPC payload**:
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "sendTransaction",
  "params": ["<base64-encoded-transaction>", {"encoding": "base64", "skipPreflight": true}]
}
```

**Validation**: HTTP 200 + response body contains `result` field with a valid base58 signature (87-88 chars). Do NOT check for a specific expected signature (each unique transaction produces a different one).

### 4.3 Data Generation Strategy (P0 Fix)

The current approach of a single hardcoded transaction is fundamentally broken. The dedup stage filters duplicates, so only the first VU's first iteration succeeds.

**Solution**: Build `k6/scripts/generate-transactions.js` (Node.js) that:

1. Generates N ephemeral Keypairs.
2. For each, creates a unique SPL Memo program transaction (each with a different memo string). Memo transactions are the simplest option because they:
   - Pass the program allowlist filter at `core/src/rpc/send_transaction_impl.rs` (spl_memo is in the whitelist)
   - Require only one signer
   - Have minimal account requirements
   - Are lightweight to construct
3. Signs each transaction with the ephemeral keypair.
4. Serializes to JSON: `[{"tx": "<base64>", "sig": "<base58>"}, ...]`
5. k6 loads via `SharedArray` — each VU picks a unique transaction using `(__VU - 1) * iterationsPerVU + __ITER` index.

**Transaction count**: Generate at least 100,000 unique transactions for stress tests. For smoke tests, 1,000 is sufficient.

**Alternative**: For pure pipeline throughput testing (ignoring dedup), generate unique transfers between different keypair pairs. However, these require funded accounts on the Contra channel.

### 4.4 Scenarios

#### Smoke (CI gate)
```typescript
scenarios: {
  smoke: {
    executor: 'constant-vus',
    vus: 5,
    duration: '10s',
  }
}
// Thresholds: p95 < 200ms, success > 99%
```

#### Load (sustained expected traffic)
```typescript
scenarios: {
  load: {
    executor: 'constant-arrival-rate',
    rate: 2000,          // 2,000 req/s
    timeUnit: '1s',
    duration: '5m',
    preAllocatedVUs: 200,
    maxVUs: 500,
  }
}
// Thresholds: p95 < 500ms, success > 95%
```

#### Stress (find breaking point)
```typescript
scenarios: {
  stress: {
    executor: 'ramping-arrival-rate',
    startRate: 100,
    timeUnit: '1s',
    stages: [
      { duration: '1m', target: 1000 },
      { duration: '1m', target: 2000 },
      { duration: '1m', target: 3000 },
      { duration: '2m', target: 4000 },
      { duration: '1m', target: 5000 },
      { duration: '1m', target: 0 },
    ],
    preAllocatedVUs: 500,
    maxVUs: 2000,
  }
}
// Thresholds: p95 < 1s, success > 90%
```

#### Soak (memory leaks, connection pool exhaustion)
```typescript
scenarios: {
  soak: {
    executor: 'constant-arrival-rate',
    rate: 1000,
    timeUnit: '1s',
    duration: '30m',
    preAllocatedVUs: 200,
    maxVUs: 500,
  }
}
// Thresholds: p95 < 500ms, success > 95%
```

#### Spike (sudden burst)
```typescript
scenarios: {
  spike: {
    executor: 'ramping-arrival-rate',
    startRate: 100,
    timeUnit: '1s',
    stages: [
      { duration: '10s', target: 100 },
      { duration: '5s',  target: 4000 },
      { duration: '30s', target: 4000 },
      { duration: '5s',  target: 100 },
      { duration: '30s', target: 100 },
    ],
    preAllocatedVUs: 500,
    maxVUs: 2000,
  }
}
// Thresholds: p95 < 2s, success > 85%
```

### 4.5 SLO Targets

| Metric | Smoke | Load | Stress | Soak | Spike |
|--------|-------|------|--------|------|-------|
| p95 latency | < 200ms | < 500ms | < 1s | < 500ms | < 2s |
| p99 latency | < 500ms | < 1s | < 2s | < 1s | < 5s |
| Success rate | > 99% | > 95% | > 90% | > 95% | > 85% |
| TPS target | 50 | 2,000 | 4,000+ | 1,000 | 4,000 burst |

### 4.6 Metrics to Collect

**Custom k6 metrics**:
- `send_tx_duration` (Trend) — Time from HTTP POST to response received
- `send_tx_success` (Rate) — Fraction with HTTP 200 + valid base58 signature
- `send_tx_rejected` (Counter) — Responses with program allowlist rejection
- `send_tx_dedup_rejected` (Counter) — Responses indicating duplicate transaction
- `pipeline_throughput` (Counter) — Successful transactions per second

**Prometheus metrics to correlate**:
- `contra_gateway_requests_total{method="sendTransaction"}` — gateway-level request count
- `contra_gateway_request_duration_seconds{method="sendTransaction"}` — gateway latency
- Node pipeline stage durations (from contra-node-metrics dashboard)
- PostgreSQL connection pool usage (from cAdvisor)

### 4.7 Environment Requirements
- Full Docker Compose stack running (or cloud deployment)
- Pre-generated transaction file with 1,000-100,000 unique transactions
- k6 installed locally or k6 Docker image
- Optional: Prometheus Remote Write endpoint configured

---

## 5. Load Test Plan: Deposit to Escrow

### 5.1 Reality Check

Deposits are **Solana on-chain operations**. The deposit flow:
1. User submits a `Deposit` instruction to the **Escrow Program** (`GokvZqD2yP696rzNBNbQvcZ4VsLW7jNvFXU1kW9m7k83`) on Solana
2. Indexer monitors Solana for deposit events (via Yellowstone gRPC or RPC polling)
3. Operator mints corresponding tokens on the Contra channel

**k6 cannot directly submit Solana transactions** — it lacks Ed25519 signing and Solana transaction serialization. A two-tier approach is needed.

### 5.2 Escrow Deposit Instruction Details

For reference, the deposit instruction (discriminator `6`) requires:

| # | Account | Signer | Writable | Description |
|---|---------|--------|----------|-------------|
| 0 | payer | Yes | Yes | Transaction fee payer |
| 1 | user | Yes | No | User depositing tokens |
| 2 | instance | No | No | Escrow instance PDA |
| 3 | mint | No | No | Token mint |
| 4 | allowed_mint | No | No | AllowedMint PDA |
| 5 | user_ata | No | Yes | User's Associated Token Account |
| 6 | instance_ata | No | Yes | Instance's Associated Token Account |
| 7-11 | programs | No | No | System, Token, ATA, event authority, self |

**Instruction data**: `amount: u64` (8 bytes) + optional `recipient: Pubkey` (1 + 32 bytes)

Source: `contra-escrow-program/program/src/processor/deposit.rs`

### 5.3 Test Architecture — Two-Tier Approach

#### Tier 1: Operator Pipeline Load Test (k6-compatible)

Test the operator's ability to process a backlog of deposit records by seeding PostgreSQL directly.

```
k6/script ──SQL INSERT──>  PostgreSQL (indexer DB)
                            transactions table
                            status = 'pending'
                            transaction_type = 'deposit'
                                  |
                                  v
                            Operator (fetcher -> processor -> sender)
                                  |
                                  v
                            Contra channel (mint tokens)
```

**Approach A — SQL seeding + monitoring**:
1. Write a SQL script (`k6/scripts/seed-deposits.sql`) that inserts N Pending deposit rows
2. Start the operator
3. k6 polls Prometheus metrics or the DB to measure drain rate

**Approach B — HTTP test harness**:
1. Build a lightweight HTTP service that exposes `POST /inject-deposit`
2. The endpoint inserts a Pending deposit row into the indexer DB
3. k6 calls this endpoint at controlled rates
4. This allows using k6's arrival-rate executors for precise load control

**Approach C — k6 SQL extension**:
1. Use `k6/x/sql` (experimental PostgreSQL support) to directly INSERT rows from k6
2. Most direct, but requires building k6 with the SQL extension

**DbTransaction schema** (from `indexer/src/storage/common/models.rs:37-56`):
```sql
INSERT INTO transactions (
  signature, trace_id, slot, initiator, recipient,
  mint, amount, memo, transaction_type, withdrawal_nonce,
  status, created_at, updated_at
) VALUES (
  'unique-sig-{N}',          -- unique per row
  'trace-{N}',               -- unique per row
  12345,                      -- valid slot number
  'initiator-pubkey',         -- Solana pubkey (base58)
  'recipient-pubkey',         -- Solana pubkey (base58)
  'mint-pubkey',              -- must exist in mints table
  1000000,                    -- amount in smallest unit
  NULL,                       -- optional memo
  'deposit',                  -- TransactionType::Deposit
  NULL,                       -- no nonce for deposits
  'pending',                  -- TransactionStatus::Pending
  NOW(), NOW()
);
```

#### Tier 2: End-to-End Deposit Load Test (Rust harness)

Use the activity generator (`core/src/bin/activity.rs`) or a custom Rust binary to:
1. Create many unique deposit transactions on Solana (via test validator)
2. Submit them to the Escrow Program
3. Measure indexer pickup latency (Solana confirmation -> DB row creation)
4. Measure operator processing latency (Pending -> Completed status)

This tier requires a running Solana test validator with the escrow program deployed.

### 5.4 Scenarios (Tier 1)

#### Backlog Drain
1. Insert 10,000 Pending deposit rows
2. Start operator
3. Measure time to drain backlog, peak processing rate, error rate
4. **Target**: drain rate > 100 deposits/second

#### Sustained Ingestion
1. k6 with `constant-arrival-rate` at 10-100 deposits/second (via Approach B or C)
2. Operator processes concurrently
3. Measure steady-state backlog depth and processing latency distribution
4. **Target**: backlog depth stays bounded (< 500)

#### Burst
1. Insert 1,000 deposits in a 1-second burst
2. Measure operator recovery time to clear the backlog
3. **Target**: full recovery within 30 seconds

### 5.5 SLO Targets

| Metric | Target |
|--------|--------|
| Operator drain rate | > 100 deposits/second |
| E2E deposit latency (Solana confirm -> Contra mint) | < 30s |
| Indexer slot processing lag | < 10 slots behind chain tip |
| Deposit loss rate | 0% (all deposits eventually processed) |
| Backlog depth under sustained load | < 500 |

### 5.6 Metrics to Collect

**Custom metrics**:
- `deposit_insertion_rate` (Counter) — Deposits injected per second
- `deposit_backlog_depth` (Gauge) — Current pending count
- `deposit_drain_rate` (Counter) — Deposits completed per second
- `deposit_e2e_latency` (Trend) — Time from insert to completed status

**Prometheus metrics to correlate**:
- `contra_operator_transactions_fetched_total{program_type="escrow"}`
- `contra_operator_mints_sent_total{program_type="escrow"}`
- `contra_operator_backlog_depth{program_type="escrow"}`
- `contra_operator_rpc_send_duration_seconds{program_type="escrow"}`
- `contra_operator_transaction_errors_total{program_type="escrow"}`

### 5.7 Environment Requirements
- PostgreSQL indexer database accessible (with `transactions` and `mints` tables populated)
- Operator service running
- For Tier 2: Solana test validator with escrow program deployed, funded user accounts
- For Approach B: Test harness HTTP service

---

## 6. Load Test Plan: Withdrawal from Escrow

### 6.1 Reality Check

Withdrawals have **two phases**:
1. **Contra-side**: User sends a withdrawal transaction via `sendTransaction` on the Contra channel (this uses the Withdraw Program ID `J231K9UEpS4y4KAPwGc4gsMNCjKFRMYcQBcjVW7vBhVi`, which is in the RPC allowlist)
2. **Solana-side**: Operator detects the withdrawal, builds a `ReleaseFunds` instruction with SMT proof, signs it, and submits it to Solana

Phase 1 is directly k6-testable (same as Scenario 1 but with withdrawal transactions). Phase 2 has the same constraints as deposits — it's Solana on-chain and requires the operator pipeline.

### 6.2 ReleaseFunds Instruction Details

The `ReleaseFunds` instruction (discriminator `7`) requires:

| # | Account | Signer | Writable | Description |
|---|---------|--------|----------|-------------|
| 0 | payer | Yes | Yes | Transaction fee payer |
| 1 | operator | Yes | No | Operator releasing funds |
| 2 | instance | No | No | Escrow instance PDA |
| 3 | operator_pda | No | No | Operator state PDA |
| 4 | mint | No | No | Token mint |
| 5 | allowed_mint | No | No | AllowedMint PDA |
| 6 | user_ata | No | Yes | Recipient token account |
| 7 | instance_ata | No | Yes | Escrow token account |
| 8-11 | programs | No | No | Token, ATA, event authority, self |

**Instruction data** (80+ bytes):
- `amount: u64` (8 bytes)
- `user: Pubkey` (32 bytes)
- `new_withdrawal_root: [u8; 32]` (32 bytes) — updated SMT root
- `transaction_nonce: u64` (8 bytes)
- `sibling_proofs: Vec<[u8; 32]>` — SMT proof path (variable length)

Source: `contra-escrow-program/program/src/processor/release_funds.rs`

### 6.3 Critical: SMT Proof & Tree Rotation

The withdrawal processor (`indexer/src/operator/processor.rs`) has important constraints:

1. **Sequential nonces**: Each withdrawal has a unique `withdrawal_nonce` that must be processed in order
2. **SMT proof generation**: Each withdrawal requires generating a Sparse Merkle Tree proof, which is computationally expensive
3. **Tree rotation**: At `MAX_TREE_LEAVES` boundaries, the processor triggers a `ResetSmtRoot` transaction. Load test data must account for this — nonces clustered around boundaries will trigger tree rotations
4. **Error handling**: `InvalidSmtProof` triggers proof regeneration and retry; `InvalidTransactionNonce` is fatal

### 6.4 Test Architecture — Three-Tier Approach

#### Tier 1: Withdrawal Submission (k6)

Test sending withdrawal transactions to the Contra channel via `sendTransaction`. Identical to Scenario 1 but with Withdraw Program transactions.

```
k6 VUs  ──HTTP POST──>  Gateway  ──>  write-node  ──>  pipeline
                         sendTransaction (Withdraw Program)
```

**Requirements**: Users must have Contra token balances (requires prior deposits).

#### Tier 2: Operator Withdrawal Pipeline (DB-seeded)

Identical approach to Deposit Tier 1, but with critical differences in the seed data:

```sql
INSERT INTO transactions (
  signature, trace_id, slot, initiator, recipient,
  mint, amount, memo, transaction_type, withdrawal_nonce,
  status, created_at, updated_at
) VALUES (
  'withdraw-sig-{N}',
  'trace-{N}',
  12345,
  'initiator-pubkey',
  'recipient-pubkey',
  'mint-pubkey',              -- must exist in mints table
  1000000,
  NULL,
  'withdrawal',               -- TransactionType::Withdrawal
  {N},                         -- SEQUENTIAL nonce (critical!)
  'pending',
  NOW(), NOW()
);
```

**Key constraint**: `withdrawal_nonce` values must be sequential. The operator expects ordered processing. Non-sequential nonces will cause failures.

#### Tier 3: End-to-End Withdrawal (Rust harness)

Full E2E flow using the activity generator or custom Rust binary.

### 6.5 Scenarios

#### Pipeline Throughput (Tier 2)
1. Pre-provision on-chain state: escrow instance, allowed mints, funded ATAs, recipient accounts
2. Insert 5,000 withdrawal rows with sequential nonces (0-4999)
3. Start operator
4. Measure drain rate, SMT proof generation overhead, tree rotation impact
5. **Target**: > 50 withdrawals/second

#### Sustained Withdrawal Flow (Tier 2)
1. Continuously inject 10-50 withdrawal rows/second with incrementing nonces
2. Measure steady-state backlog and latency
3. **Target**: backlog depth stays bounded (< 200)

#### Tree Rotation Stress (Tier 2)
1. Set nonces to cluster around `MAX_TREE_LEAVES` boundaries
2. Verify that `ResetSmtRoot` does not cause pipeline stalls or cascading failures
3. Measure rotation duration and recovery time

#### Contra-Side Submission Throughput (Tier 1)
1. Generate unique withdrawal transactions (requires users with balances)
2. Submit via k6 with `ramping-arrival-rate`
3. Measure acceptance rate and latency (same SLOs as Scenario 1)

### 6.6 SLO Targets

| Metric | Target |
|--------|--------|
| Withdrawal submission latency (Contra RPC) | p95 < 500ms |
| Operator drain rate | > 50 withdrawals/second |
| SMT proof generation time | < 100ms per proof |
| E2E withdrawal latency (Contra burn -> Solana release) | < 60s |
| Tree rotation duration | < 5s |
| Withdrawal loss rate | 0% |

### 6.7 Metrics to Collect

**Custom metrics**:
- `withdrawal_submission_duration` (Trend) — Contra RPC submission latency
- `withdrawal_operator_drain_rate` (Counter) — Withdrawals completed per second
- `withdrawal_smt_proof_duration` (Trend) — Time for SMT proof generation
- `withdrawal_tree_rotation_duration` (Trend) — Time for ResetSmtRoot transactions
- `withdrawal_e2e_latency` (Trend) — Total time from Contra burn to Solana release

**Prometheus metrics to correlate**:
- `contra_operator_transactions_fetched_total{program_type="withdraw"}`
- `contra_operator_rpc_send_duration_seconds{program_type="withdraw"}`
- `contra_operator_transaction_errors_total{program_type="withdraw"}`
- `contra_operator_backlog_depth{program_type="withdraw"}`
- `contra_feepayer_balance_lamports{program_type="escrow"}`

### 6.8 Environment Requirements
- Full stack with escrow instance initialized and funded
- Mints whitelisted in escrow program
- Operator keypairs configured (admin, operator)
- For Tier 1: Users with Contra token balances (requires prior deposits)
- For Tier 2: Pre-provisioned on-chain escrow state + seeded DB with sequential nonces
- Sufficient feepayer SOL balance (monitored via `contra_feepayer_balance_lamports`)

---

## 7. Shared Infrastructure Improvements

### 7.1 Proposed Directory Structure

```
k6/
├── src/
│   ├── scenarios/
│   │   ├── transfers.ts              # Scenario 1: Contra transfers (refactored send-transaction)
│   │   ├── transfers-stress.ts       # Scenario 1: Stress/throughput variant
│   │   ├── deposit-pipeline.ts       # Scenario 2: Deposit operator pipeline
│   │   ├── withdrawal-pipeline.ts    # Scenario 3: Withdrawal operator pipeline
│   │   └── gateway-routing.ts        # Bonus: Gateway routing under mixed load
│   └── lib/
│       ├── rpc.ts                    # JSON-RPC helper (sendTransaction, getSlot, etc.)
│       ├── metrics.ts                # Shared custom metric definitions
│       ├── config.ts                 # Environment config, thresholds, URLs
│       ├── data.ts                   # SharedArray loaders for transaction data
│       ├── checks.ts                 # Common check functions (valid signature, etc.)
│       └── summary.ts               # handleSummary for CI-friendly JSON output
├── scripts/
│   ├── generate-transactions.js      # Node.js: generate unique signed transactions
│   ├── seed-deposits.sql             # SQL: insert pending deposit rows
│   ├── seed-withdrawals.sql          # SQL: insert pending withdrawal rows (sequential nonces)
│   ├── run-smoke.sh                  # CI smoke test runner
│   ├── run-load.sh                   # Full load test runner
│   └── run-stress.sh                 # Stress test runner
├── data/                             # Generated test data (gitignored)
├── results/                          # Test results (gitignored)
├── dist/                             # Compiled k6 scripts (gitignored)
├── webpack.config.js
├── tsconfig.json
├── package.json
└── README.md
```

### 7.2 Prometheus Remote Write Integration

Configure k6 to emit metrics directly to Prometheus for real-time correlation with application metrics:

```bash
k6 run \
  --out experimental-prometheus-rw \
  -e K6_PROMETHEUS_RW_SERVER_URL=http://prometheus:9090/api/v1/write \
  -e K6_PROMETHEUS_RW_TREND_AS_NATIVE_HISTOGRAM=true \
  dist/scenarios/transfers.js
```

This enables a single Grafana dashboard showing k6 metrics alongside application metrics.

### 7.3 Docker Compose Integration

Add to `docker-compose.yml` (or a separate `docker-compose.loadtest.yml`):

```yaml
k6:
  image: grafana/k6:latest
  container_name: contra-k6
  volumes:
    - ./k6/dist:/scripts:ro
    - ./k6/data:/data:ro
    - ./k6/results:/results
  environment:
    - RPC_URL=http://gateway:${GATEWAY_PORT}
    - K6_PROMETHEUS_RW_SERVER_URL=http://prometheus:9090/api/v1/write
  command: run --out experimental-prometheus-rw /scripts/scenarios/transfers.js
  networks:
    - contra-network
  profiles:
    - loadtest
```

Usage: `docker compose --profile loadtest up k6`

### 7.4 CI Integration

Add a smoke test job to `.github/workflows/rust.yml`:

```yaml
load-test-smoke:
  name: K6 Smoke Test
  needs: [integration-tests]
  runs-on: contra-runner-1
  steps:
    - uses: actions/checkout@v4
    - uses: actions/setup-node@v4
      with:
        node-version: '20'
    - name: Install k6
      run: |
        curl -s https://dl.k6.io/key.gpg | sudo apt-key add -
        echo "deb https://dl.k6.io/deb stable main" | sudo tee /etc/apt/sources.list.d/k6.list
        sudo apt-get update && sudo apt-get install -y k6
    - name: Generate test transactions
      run: cd k6 && node scripts/generate-transactions.js --count 1000
    - name: Build k6 scripts
      run: cd k6 && npm ci && npm run build
    - name: Run smoke test
      run: |
        cd k6 && k6 run \
          --summary-export=results/smoke-summary.json \
          -e RPC_URL=http://localhost:8899 \
          dist/scenarios/transfers.js
    - uses: actions/upload-artifact@v4
      if: always()
      with:
        name: k6-smoke-results
        path: k6/results/
```

### 7.5 Grafana Load Test Dashboard

Create a new dashboard `grafana/dashboards/contra-load-test.json` that combines:
- k6 metrics (via Prometheus Remote Write): request rate, latency percentiles, success rate, VU count
- Gateway metrics: `contra_gateway_requests_total`, `contra_gateway_request_duration_seconds`
- Pipeline metrics: stage durations, batch sizes
- Operator metrics: backlog depth, drain rate, errors
- System metrics: CPU, memory, network (from cAdvisor)

---

## 8. Implementation Priority

### Phase 1: Fix Fundamentals (Week 1)

| # | Task | Details |
|---|------|---------|
| 1 | Build transaction generator | `k6/scripts/generate-transactions.js` — generate unique Memo program transactions. Without this, no k6 test produces valid results. |
| 2 | Refactor `send-transaction.ts` | Use `SharedArray` for generated transactions, switch to `scenarios` API, fix signature validation to accept any valid base58 signature, add proper tagging. |
| 3 | Create shared library | `k6/src/lib/` — extract RPC helper, metrics, config, checks, summary. |
| 4 | Update README | Fix stale references, document new workflow. |

### Phase 2: Expand Coverage (Week 2)

| # | Task | Details |
|---|------|---------|
| 5 | Gateway load test | `k6/src/scenarios/gateway-routing.ts` — test both write (`sendTransaction`) and read (`getSlot`, `getAccountInfo`) paths under mixed load. |
| 6 | Deposit pipeline test | `k6/src/scenarios/deposit-pipeline.ts` + `k6/scripts/seed-deposits.sql` — implement Tier 1 with DB seeding. |
| 7 | Withdrawal pipeline test | `k6/src/scenarios/withdrawal-pipeline.ts` + `k6/scripts/seed-withdrawals.sql` — implement Tier 1 with sequential nonce seeding. |

### Phase 3: Observability & CI (Week 3)

| # | Task | Details |
|---|------|---------|
| 8 | Prometheus Remote Write | Configure k6 output to Prometheus. |
| 9 | Grafana load test dashboard | Create `contra-load-test.json` correlating k6 + app metrics. |
| 10 | CI smoke test | Add k6 smoke job to GitHub Actions. |
| 11 | Docker Compose integration | Add k6 service with `loadtest` profile. |

### Phase 4: Advanced (Week 4+)

| # | Task | Details |
|---|------|---------|
| 12 | Soak test | 30+ minute sustained load — focus on memory leaks, connection pool exhaustion, PostgreSQL WAL growth. |
| 13 | E2E deposit/withdrawal | Tier 2/3 using activity generator or custom Rust harness for real on-chain transactions. |
| 14 | Tree rotation stress | Test withdrawal nonces at `MAX_TREE_LEAVES` boundaries. |
| 15 | Chaos testing | Combine load with node restarts, database failovers, network partitions. |

---

## Appendix: Key File References

| File | Relevance |
|------|-----------|
| `k6/src/send-transaction.ts` | Existing basic load test (to refactor) |
| `k6/src/max-throughput.ts` | Existing stress test (to refactor) |
| `k6/webpack.config.js` | Build config (add new entry points) |
| `k6/run.sh` | Shell runner (update for new structure) |
| `core/src/rpc/send_transaction_impl.rs` | RPC entry point with program allowlist (lines 72-82) |
| `core/src/bin/activity.rs` | Activity generator (reference for tx construction) |
| `indexer/src/storage/common/models.rs:37-56` | DbTransaction schema (for SQL seeding) |
| `indexer/src/operator/processor.rs` | Withdrawal processor (SMT proof, tree rotation) |
| `indexer/src/operator/fetcher.rs` | Operator fetcher (SELECT FOR UPDATE SKIP LOCKED) |
| `indexer/src/metrics.rs` | Operator/indexer Prometheus metrics |
| `gateway/src/metrics.rs` | Gateway Prometheus metrics |
| `contra-escrow-program/program/src/processor/deposit.rs` | Deposit instruction |
| `contra-escrow-program/program/src/processor/release_funds.rs` | ReleaseFunds instruction |
| `grafana/dashboards/` | Existing Grafana dashboards |
| `docker-compose.yml` | Full stack definition |
