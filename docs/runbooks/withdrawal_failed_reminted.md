# Runbook - Withdrawal `FailedReminted`

Triggered by webhook `status=failed_reminted`. **This is a success outcome.**
The original withdrawal failed on Solana; the channel-side remint restored
the user's burned private channel tokens.

## Symptom

- Webhook payload: `status=failed_reminted`, `remint_signature=<sig>`,
  `remint_status=success`.
- ERROR log line `Transaction <id> FailedReminted` (the writer logs every
  alertable status at ERROR for paging visibility - this one is benign).

No funds are stranded. No recovery action needed.

## Reconciliation steps

1. **Confirm the remint signature on-chain.**
   ```bash
   solana confirm -v <remint_signature> --url <rpc-url>
   ```
   Expected: `Finalized` with no error. The remint targets the private channel side
   token program, so use the channel read node's RPC.
2. **Confirm the original withdrawal did NOT land** by running
   [`_verify_onchain_release.md`](_verify_onchain_release.md). Expected
   verdict: `NOT_LANDED`. If `LANDED`, the user has been double-credited
   (got funds on Solana AND a remint) -
   [escalate immediately](_escalation.md) (Tier 1).
3. **Close the alert** in your tracker. Note the remint signature.

## When to investigate further

- If `error_message` indicates a transient cause (RPC timeout, blockhash
  expiry) the original withdrawal may have been recoverable without remint.
  Repeated occurrences suggest the retry/backoff config or the
  `RetryPolicy::None` decision in `sender/remint.rs::attempt_remint` needs
  revisiting.
- If multiple `failed_reminted` events fire within a short window, file a
  ticket - the underlying RPC or program is unstable.

## Post-incident

Capture: transaction_id, original `error_message`, remint_signature, and the
on-chain verdict for both. These feed any reconciliation reports.
