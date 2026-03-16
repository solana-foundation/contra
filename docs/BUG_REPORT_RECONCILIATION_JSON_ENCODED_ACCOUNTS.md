# Bug Report: Reconciliation Loop Silently Ignores All On-Chain Token Balances

**Severity**: High
**Component**: `contra-indexer` — operator reconciliation (`indexer/src/operator/reconciliation.rs`)
**Status**: Fixed
**Discovered**: 2026-03-18
**Fixed**: 2026-03-18

---

## Summary

The periodic reconciliation loop in the operator never fired webhook alerts for balance mismatches because `fetch_on_chain_balances` silently failed on every call. The function attempted to binary-decode token account data returned by the Solana RPC, but the RPC returns accounts in JSON-parsed format (`UiAccountData::Json`), for which `decode()` returns `None`. The resulting error was swallowed by a `warn!` in the reconciliation loop, leaving the safety check completely inoperative.

---

## Background

The reconciliation loop (`run_reconciliation`) is the primary safety invariant for the escrow system (invariant G1 / O3). It runs on a configurable interval, compares the sum of completed deposits minus withdrawals in the database against the actual token balances held in the escrow's Associated Token Accounts (ATAs) on Solana, and fires a webhook alert if the delta exceeds a configured tolerance. A failure here means stolen or corrupted funds could go undetected indefinitely.

---

## Root Cause

### The call chain

```
run_reconciliation
  └── perform_reconciliation_check
        └── fetch_on_chain_balances          ← bug here
              └── rpc_client.get_token_accounts_by_owner(...)
```

### The encoding mismatch

`solana_client::RpcClient::get_token_accounts_by_owner` internally requests `UiAccountEncoding::JsonParsed` from the RPC node. The node returns account data as the `UiAccountData::Json(UiParsedAccount)` variant, not as a base64-encoded binary blob.

The old code called `keyed_account.account.data.decode()` unconditionally:

```rust
// indexer/src/operator/reconciliation.rs (before fix)
for keyed_account in accounts {
    let account_data = keyed_account.account.data.decode().ok_or_else(|| {
        OperatorError::RpcError("Failed to decode token account data".to_string())
    })?;
    let token_account = TokenAccount::unpack(&account_data)...?;
    *balances.entry(token_account.mint).or_insert(0) += token_account.amount;
}
```

`UiAccountData::decode()` is only implemented for the `Binary` and `LegacyBinary` variants — it returns `None` for `Json`. This caused `ok_or_else` to produce an `Err` for every single token account returned, making `fetch_on_chain_balances` always fail.

### The silent swallow

`perform_reconciliation_check` propagates the error up to `run_reconciliation`, which catches it with:

```rust
// run_reconciliation (before fix)
if let Err(e) = perform_reconciliation_check(...).await {
    warn!("Reconciliation check failed: {}", e);
    // loop continues — no webhook, no panic
}
```

`warn!` is a no-op unless a tracing subscriber is initialised. Integration tests do not initialise tracing by default. The error was therefore completely invisible in test output. In production, even with tracing enabled, a `warn!` would not alert operators — it would just fill logs with repeated warnings while the safety check silently never ran.

---

## Impact

- **Balance mismatch alerts were never delivered**, regardless of the actual on-chain vs. DB delta.
- The `reconciliation_webhook_url` configuration option had no effect.
- The `reconciliation_tolerance_bps` threshold was never evaluated.
- Any theft, corruption, or accounting error in the escrow would have gone undetected by this mechanism.

---

## Affected Versions

All versions prior to the fix on 2026-03-18. The bug was present since `fetch_on_chain_balances` was introduced.

---

## Fix

**File**: `indexer/src/operator/reconciliation.rs`
**File**: `indexer/Cargo.toml`

### 1. Added dependency

```toml
# indexer/Cargo.toml
solana-account-decoder-client-types = { workspace = true }
```

### 2. Added imports

```rust
use solana_account_decoder_client_types::UiAccountData;
use std::str::FromStr;
```

### 3. Replaced binary-only decode with a variant-aware match

The parsing loop now handles both encoding variants:

```rust
for keyed_account in accounts {
    let (mint, amount) = match &keyed_account.account.data {
        // Binary encoding: decode base64, then unpack the SPL token layout
        data if data.decode().is_some() => {
            let account_data = data.decode().unwrap();
            let token_account = TokenAccount::unpack(&account_data)
                .map_err(|e| OperatorError::RpcError(...))?;
            (token_account.mint, token_account.amount)
        }
        // JSON-parsed encoding: the RPC has already decoded the account;
        // extract mint and amount from the nested `info` object.
        UiAccountData::Json(parsed) => {
            let info = parsed.parsed.get("info")
                .ok_or_else(|| OperatorError::RpcError(...))?;
            let mint_str = info.get("mint")
                .and_then(|v| v.as_str())
                .ok_or_else(|| OperatorError::RpcError(...))?;
            let amount_str = info.get("tokenAmount")
                .and_then(|v| v.get("amount"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| OperatorError::RpcError(...))?;
            let mint = Pubkey::from_str(mint_str).map_err(...)?;
            let amount = amount_str.parse::<u64>().map_err(...)?;
            (mint, amount)
        }
        // Unknown encoding — skip with a warning rather than hard-failing
        _ => {
            warn!("Skipping token account with unrecognised data encoding...");
            continue;
        }
    };
    *balances.entry(mint).or_insert(0) += amount;
}
```

The JSON-parsed path extracts data from the structure the Solana RPC actually returns:

```json
{
  "info": {
    "mint": "<base58 pubkey>",
    "tokenAmount": {
      "amount": "12345",
      "decimals": 6,
      "uiAmount": 0.012345
    }
  }
}
```

---

## Verification

A new integration test was added to `integration/tests/indexer/operator_lifecycle.rs`:

```
test_periodic_reconciliation_fires_webhook_on_mismatch
```

**Test approach**:
1. Start a Solana test validator and a fresh PostgreSQL container.
2. Call `AllowMint` on the escrow program to create an ATA with 0 on-chain balance.
3. Insert a completed deposit of 50,000 tokens into the DB — creating a guaranteed mismatch.
4. Start a mockito HTTP server and configure `reconciliation_webhook_url` to point to it.
5. Invoke `run_reconciliation` directly (bypassing `operator::run`'s `ctrl_c()` gate) with `reconciliation_interval = 500ms` and `reconciliation_tolerance_bps = 0`.
6. After 3 seconds, cancel the reconciliation task and assert the mock received at least one POST.

The test passed in 32 seconds after the fix was applied.

---

## Follow-up Recommendations

1. **Escalate reconciliation errors**: replace the `warn!` in the `run_reconciliation` catch-all with an `error!` and consider sending an alert even when the reconciliation check itself fails — a failing check is itself a signal worth surfacing.
2. **Add unit tests for `fetch_on_chain_balances`**: mock the RPC response with `UiAccountData::Json` to prevent regression.
3. **Audit other RPC call sites** for the same pattern of assuming binary encoding when `get_token_accounts_by_owner` or similar calls are used.
