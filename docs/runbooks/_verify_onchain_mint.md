# Procedure - Verify On-Chain Mint

**Scope:** deposit operator only. This procedure verifies whether a
`MintTo` instruction landed on the **private channel chain** (the channel side, not
Solana mainnet). For withdrawals, see
[`_verify_onchain_release.md`](_verify_onchain_release.md).

Run this before taking any deposit recovery action. **Do not skip - this
is the gate that prevents double-minting private channel tokens to the user.**

## Inputs

You need:
- `transaction_id` - DB primary key of the deposit row.
- The recipient ATA - derivable from `recipient` and `mint` columns of the
  row, or already in `counterpart_signature`'s associated transaction.
- A working RPC endpoint for the **private channel chain** (the channel, served by
  the gateway / read-node, not the Solana mainnet RPC). The operator's
  `COMMON_RPC_URL` env var is the same endpoint.

## Output

Exactly one of:
- `LANDED <signature>` - mint confirmed on the private channel; tokens were minted.
- `NOT_LANDED` - no mint in operator history for this transaction_id.
- `AMBIGUOUS` - RPC unreachable, no decisive evidence, or the lookback
  window does not cover `processed_at`.

**If output is `AMBIGUOUS`, stop. [Escalate](_escalation.md) (Tier 2).
Do not retry.** A blind retry
risks double-minting if the original mint actually landed but is outside
the RPC's signature lookback window.

## Procedure

### Step 1 - pull row state

```sql
SELECT id,
       recipient,
       mint,
       counterpart_signature,
       status,
       updated_at
  FROM transactions
 WHERE id = :transaction_id;
```

If `counterpart_signature` is set, the operator already recorded a mint
sig - verify it directly in Step 2 and skip Step 3.

### Step 2 - confirm a known signature

```bash
solana confirm -v <counterpart_signature> --url <private-channel-rpc-url>
```

- `Finalized`, no error → output `LANDED <counterpart_signature>`.
- `Failed` or `not found` → continue to Step 3 (the recorded sig may have
  been speculative; the actual mint may differ).
- RPC error → output `AMBIGUOUS`.
  [Escalate](_escalation.md) (Tier 2).

### Step 3 - search by idempotency memo

The operator attaches a deterministic memo to every mint:
`private_channel:mint-idempotency:<transaction_id>`
(`indexer/src/operator/constants.rs::MINT_IDEMPOTENCY_MEMO_PREFIX`).

Derive the recipient ATA:

```bash
spl-token address \
  --token <mint> \
  --owner <recipient> \
  --url <private-channel-rpc-url> \
  --verbose
```

Scan recent signatures on that ATA:

```bash
solana transaction-history <recipient-ata> --limit 1000 --url <private-channel-rpc-url>
```

For each candidate, fetch and inspect:

```bash
solana confirm -v <signature> --url <private-channel-rpc-url>
```

A match has all of:
- A `MintTo` instruction targeting the same mint and recipient ATA.
- A memo instruction whose data contains
  `private_channel:mint-idempotency:<transaction_id>`.
- `Finalized` commitment, no error.

Outcomes:
- One match → output `LANDED <signature>`. Use this signature in
  recovery.
- No match within the lookback window AND `processed_at` is more recent
  than the oldest signature returned → output `NOT_LANDED`.
- No match BUT `processed_at` predates the oldest signature returned →
  output `AMBIGUOUS` (the original mint may have rotated out of the RPC's
  history window). [Escalate](_escalation.md) (Tier 2).
- RPC unreachable → output `AMBIGUOUS`.

## Idempotency safety net

The operator's `find_existing_mint_signature_with_memo`
(`indexer/src/operator/sender/mint.rs`) runs the same memo-scan on every
deposit attempt before sending. If you re-arm a deposit row to `pending`
without recovery and the original mint did land, the next operator tick
will find the existing memo'd signature and short-circuit to `Completed`
without minting again.

This safety net works only when the original mint is still inside the
RPC's signature lookback window. **It is the primary defense against
double-minting on retry, but it is not a substitute for this procedure
in `AMBIGUOUS` cases.**

## After running this procedure

Capture the verdict, the signature(s) checked, and the RPC endpoint used
in the incident record. Without this trail, a future user dispute or
reconciliation mismatch cannot tell whether a row's
`counterpart_signature` was actually verified on the private channel or hand-picked,
and a postmortem cannot reproduce the recovery decision.
