# Runbook - Reconciliation Halt

This runbook covers the **runtime reconciliation halt** in the escrow operator.
Unlike the alert-only behavior of the past, the operator now fails closed: it sets
a durable DB flag that freezes **both** operators' fetchers (deposits and
withdrawals), quarantines active withdrawals, forces the escrow operator's
`/health` to 503, and posts a webhook.

A halt is deliberate: it trades liveness for integrity. Two conditions trip it,
and they confirm differently:

| Condition | Reason substring | Confirmation |
|---|---|---|
| **Insolvency** - channel-token supply exceeds escrow custody beyond the in-flight envelope | `short of supply by` | three consecutive finalized-read ticks |
| **Stale mint authority** - a channel mint no longer names the operator's configured admin | `names mint_authority` | first observation |

Insolvency needs repetition because in-flight activity or a one-off bad RPC read
can produce a gap that is not real. An authority is one pubkey compared against
another, so there is nothing for a second tick to confirm.

Recovery is **manual** for both - a human must confirm real backing, or restore
the authority, before clearing the flag.

As with every runbook here, the recovery `UPDATE` statements are
**bookkeeping, not fund movement** - see
[`README.md`](README.md) § "Recovery SQL is bookkeeping; fund restoration is
human-in-the-loop".

---

## What the operator does automatically

Runtime reconciliation reads each channel mint once per tick at **finalized** and
checks two invariants against it:

> **1. channel `Mint.supply` (PrivateChannel) must not exceed escrow custody (Solana).**
>
> **2. every initialized channel mint must name the operator's configured admin as
> its mint and freeze authority.**

On-chain supply is already net of burns (the program burns channel tokens at
withdrawal initiation, not at release), so `supply <= custody` is a pure
economic-backing check that does **not** trust the DB ledger. The DB is read only
to (a) enumerate the mint universe (the `mints` table, so a blocked or not-held
mint with outstanding supply is still checked) and (b) compute the per-mint
in-flight envelope; neither is compared against custody.

The second invariant catches the state that precedes an unbacked issuance rather
than its result. A key other than the configured admin holding mint authority can
issue channel tokens against live custody, and burning them through the
permissionless withdraw program turns them into a real escrow release. Invariant
1 does see that eventually, but only once custody has already left, so it reports
a loss already taken. Invariant 2 fires on the authority itself.

It is a periodic check, not a guard on the mint path, so a mint whose authority
changes mid-interval is caught on the next tick rather than instantly. The first
tick runs at operator startup, which is what makes it tight for the case it exists
for: a rotation that updated the configuration without migrating the mints.

An **absent** freeze authority is accepted. A foreign key there could freeze any
holder's account, but no key at all means the capability does not exist, and
halting on it would be unclearable since a freeze authority cannot be reinstalled.
An absent **mint** authority is not accepted: it bricks the mint.

When a mint shows `supply - custody` greater than its in-flight envelope (plus a
small bps cushion) for three consecutive ticks, or names an authority other than
the configured admin on any tick, the operator **halts**:

1. Sets the durable `reconciliation_halt` flag (reason recorded).
2. Quarantines every active (`pending`/`processing`/`parked`) withdrawal to
   `manual_review`.
3. Forces its `/health` to 503 (pages orchestration).
4. Posts the halt webhook. This webhook is the only alert - there is no separate
   sensitive alert layer; the alert fires together with the halt.

The halt flag is read at the top of the shared fetcher loop, so it freezes
**both** the escrow and withdraw operators, and it **survives restarts** - both
re-read it at first poll and stay frozen until it is cleared.

## Symptom

- Deposits stop minting and withdrawals stop releasing across both operators.
- The escrow operator's `/health` returns 503 with `"reason":"forced"`.
- Logs carry one of:
  - `RECONCILIATION HALT tripped; freezing both pipelines` with the mint, the
    supply gap, envelope, tolerance, and tick count.
  - `MINT AUTHORITY HALT tripped; freezing both pipelines` with the mint, the
    authorities it names, and the configured admin it was compared against.
- Active withdrawals are in `manual_review`.

## Detection

Inspect the flag directly:

```sql
SELECT halted, reason, halted_at FROM reconciliation_halt WHERE id = TRUE;
```

A `halted = TRUE` row is an active halt. Route on `reason`:

- contains `short of supply by` → insolvency. Continue to
  [§ Investigate before clearing](#investigate-before-clearing). `reason` carries
  the offending mint and the exact custody / supply-gap / envelope / tolerance
  numbers.
- contains `names mint_authority` → stale authority. Skip to
  [§ Halt reason: stale mint authority](#halt-reason-stale-mint-authority).
  Custody is **not** in question and the insolvency procedure below does not
  apply.

## Investigate before clearing

Do **not** clear the flag until you have confirmed real backing. For the mint in
the halt reason:

1. **On-chain Solana custody.** Sum the escrow instance's token accounts for the
   mint (`getTokenAccountsByOwner` on the escrow PDA), at `finalized`. This is
   authoritative custody. See [`_verify_onchain_release.md`](_verify_onchain_release.md).
2. **On-chain PrivateChannel supply.** Read the channel mint's `Mint.supply`
   (`getAccountInfo` on the mint, decode the SPL Mint). This is the total minted,
   already net of burns. `supply - custody` is the halt gap.
3. **In-flight envelope.** Confirm the gap is not merely un-settled work:

   ```sql
   SELECT mint, COALESCE(SUM(amount),0) AS in_flight
   FROM transactions
   WHERE mint = '<MINT>'
     AND status IN ('pending','processing','parked','pending_remint')
   GROUP BY mint;
   ```

4. **DB ledger (context only, not the halt basis).** The recorded deposits and
   completed withdrawals help explain *why* the supply may be unbacked (e.g. no
   deposits justify the minted amount), but the halt does not compare them:

   ```sql
   SELECT mint,
          SUM(CASE WHEN transaction_type='deposit' AND status='completed'
                   THEN amount ELSE 0 END)   AS deposits_completed,
          SUM(CASE WHEN transaction_type='withdrawal' AND status='completed'
                   THEN amount ELSE 0 END)   AS withdrawals_completed
   FROM transactions WHERE mint = '<MINT>' GROUP BY mint;
   ```

If `supply > custody` persists beyond the in-flight envelope once the in-flight
work settles, this is a **real** solvency incident (operator-key over-issuance or
a custody shortfall). Escalate per [`_escalation.md`](_escalation.md); rotate the
operator/admin key per
[`admin_rotation_runbook.md`](admin_rotation_runbook.md) if over-issuance is
suspected, and do **not** resume the pipelines.

## Halt reason: stale mint authority

The reason names one channel mint, the `mint_authority` and `freeze_authority` it
carries on the channel, and the configured admin they were compared against.
Custody is untouched and nothing has been over-issued yet. What has happened is
that a key other than the one this operator signs with can issue channel tokens
for a mint the escrow still backs.

**Do not clear the flag to make this go away.** It re-trips on the next tick, and
clearing it without restoring the authority is precisely what lets an unbacked
issuance reach a real escrow release.

Read the named mint on the PrivateChannel RPC:

```bash
spl-token display <MINT> --url <private-channel-rpc>
```

Compare `Mint authority` and `Freeze authority` to the pubkey in the halt reason.
The operator logs what it expects once at startup, as
`Expected channel mint authority: <pubkey>`.

Then match the cause:

| Authority on the channel | Cause | Action |
|---|---|---|
| The organisation's previous admin key | An admin rotation was started and not finished, or was never migrated at all. | Complete the rotation per [`admin_rotation_runbook.md`](admin_rotation_runbook.md). The migration step is what clears this. |
| A key nobody recognises | The mint was initialized on the channel by a third party before the operator got to it. | Escalate (Tier 2) per [`_escalation.md`](_escalation.md). Do not attempt migration: the signature needed to move the authority is not ours. Outstanding channel tokens for this mint were not operator-issued. |
| `none` (cleared) | The authority was removed via `SetAuthority`. No key can mint this token again, and none can be installed. | Escalate (Tier 1). The mint is permanently unusable; recovery is the coordinated mint replacement in [`deposit_manual_review.md`](deposit_manual_review.md) § `NOT_LANDED`. |

Only the first row is self-service. Once every channel mint names the configured
admin, re-run the verification step in the rotation runbook and clear the flag
with the SQL below.

## Recover (only after backing is confirmed)

For an **insolvency** halt, confirm backing first (§ Investigate before clearing).
For a **stale authority** halt, restore the authority first (§ Halt reason: stale
mint authority). Clearing the flag is the same for both.

Once you have verified that custody genuinely backs the minted supply (e.g. the
gap was a transient the reads have since settled, or the discrepancy has been
reconciled on-chain), clear the flag:

```sql
UPDATE reconciliation_halt SET halted = FALSE, halted_at = NOW() WHERE id = TRUE;
```

Both operators' fetchers resume on their next poll (no restart required). The
escrow operator's forced-unhealthy latch is in-memory, so restart it (or let the
supervisor cycle it) to clear the 503 once the flag is down. Re-queue any rows
left in `manual_review` per the relevant per-row runbook
([`withdrawal_manual_review.md`](withdrawal_manual_review.md)) after confirming
each is safe.
