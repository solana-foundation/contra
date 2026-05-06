# Runbook - Deposit `ManualReview`

Triggered by webhook payload `status=manual_review` for a row with
`transaction_type='deposit'`.

## Scope and key differences vs. withdrawals

Deposits never halt the pipeline. The processor's classifier
(`processor.rs::classify_processor_error`) is shared with the withdrawal
side, but the deposit loop continues after each quarantine
(`process_deposit_funds`, `processor.rs:704-722` - note the absence of
`halt_withdrawal_pipeline` and the loop `continue` semantics). There is
no SMT, no nonce, no remint.

Practically: a single deposit `manual_review` is a single row in trouble.
Other deposits keep flowing. There is no collateral, no sweep, no halt to
recover from.

## Symptom

- Webhook with `status=manual_review`, `transaction_type=deposit`.
- ERROR-level log line `Transaction <id> ManualReview`.

## Triage

There is one trigger surface: a deterministic per-row error in
`processor.rs::process_deposit_funds`. `error_message` will contain one
of:

| `error_message` contains | Cause |
|---|---|
| `invalid_pubkey` | `mint` or `recipient` field is not a valid base58 pubkey. |
| `invalid_builder` | Builder rejected the row's data (e.g. negative amount). |
| `program_error` | Generic builder error not covered by the specific variants. |

Pull the row:

```sql
SELECT id, signature, recipient, mint, amount, slot, updated_at
  FROM transactions
 WHERE id = :transaction_id;
```

`signature` is the originating Solana deposit signature (immutable
reference). Use it to inspect the on-chain deposit if you need to confirm
what was actually deposited:

```bash
solana confirm -v <signature> --url <solana-rpc-url>
```

## Recovery

Deposits do not need on-chain mint verification before recovery - the
quarantine triggers on row-data validation, before any RPC call. The
idempotency memo (`contra:mint-idempotency:<transaction_id>`) prevents
double-mint on retry even if the mint somehow did land.

That said: **if `error_message` is `program_error`** the trigger is less
specific and may indicate a real on-chain rejection. In that case run
[`_verify_onchain_mint.md`](_verify_onchain_mint.md) before deciding.

### Path A - bad data, unrecoverable

The row's `mint` or `recipient` is malformed beyond fixing (e.g. the
indexer captured corrupt input). Mark `failed`; refund out-of-band.

```sql
UPDATE transactions SET status = 'failed', updated_at = NOW()
 WHERE id = :transaction_id;
```

The user's tokens are locked in escrow on Solana but no Contra-side
mint will be issued. [Escalate](_escalation.md) (Tier 1) for refund
coordination -
typically a manual `release_funds` back to the depositor.

### Path B - data correctable

Rare; happens when `mint` or `recipient` was canonically wrong but the
underlying intent is recoverable from the originating Solana transaction.
Correct the columns and re-arm:

```sql
UPDATE transactions
   SET status = 'pending',
       mint = :corrected_mint,
       recipient = :corrected_recipient,
       updated_at = NOW()
 WHERE id = :transaction_id;
```

No operator restart required. The fetcher will pick the row up on its
next tick.

### Path C - conservative classification

If `error_message` describes a transient condition (RPC error, DB error
surfaced as `OperatorError::Program`), the classifier in
`classify_processor_error` quarantined on the side of caution rather
than retrying. This is the intended behavior: misclassifying a
deterministic error as transient could put the operator into a tight
retry loop. The asymmetric cost favors a noisy quarantine over a silent
retry.

Re-arm to `pending` (safe - idempotency memo prevents duplicate mint),
then [escalate](_escalation.md) (Tier 3) so the taxonomy can be
extended to classify this error variant explicitly. Do not patch
in-place.

```sql
UPDATE transactions SET status = 'pending', updated_at = NOW()
 WHERE id = :transaction_id;
```

## Post-incident artifacts

- Transaction id, originating Solana `signature`, `recipient`, `mint`.
- Full webhook `error_message`.
- Recovery action taken.
- If Path A: refund tracking ticket.

