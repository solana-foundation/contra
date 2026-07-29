# Runbook - Admin Key Rotation

`SetNewAdmin` changes `Instance.admin` and nothing else. It does not move the SPL
mint and freeze authorities on the channel mints, and those are what let a key
issue channel tokens. Skip the migration step and the old key can still issue
tokens for every mint it created, which convert to real escrow funds through the
permissionless withdraw program.

Requires downtime: the operator signs with one key at a time, so any mint whose
authority has moved is unusable until the configuration catches up.

---

## The constraint

SPL Token accepts `SetAuthority` only from the **current** authority, so the new
admin cannot migrate the mints to itself. Every migration step is signed by the
old key.

> **Do not destroy or lose the old key until step 8 passes.** An unmigrated mint
> with no reachable authority is permanently unusable; recovery is a coordinated
> mint replacement ([`deposit_manual_review.md`](deposit_manual_review.md)
> § `NOT_LANDED`).

## Steps

**1. Capture the mint list.** Step 9 must re-allow exactly this set.

```sql
SELECT mint_address FROM mints ORDER BY mint_address;
```

**2. Block every mint.** `BlockMint` each one, signed by the old admin. Closes the
`AllowedMint` PDA that `ReleaseFunds` requires, stopping deposits and releases.

**3. Drain in-flight work.** Proceed only when this returns nothing; a withdrawal
landing mid-migration fails on mint authority and ends in `manual_review`.

```sql
SELECT status, transaction_type, COUNT(*) FROM transactions WHERE status IN ('pending','processing','parked','pending_remint') GROUP BY status, transaction_type;
```

**4. Dry-run the migration.** Sends nothing; prints one line per mint.

```bash
cargo run --bin migrate_mint_authority -- --database-url "$DATABASE_URL" --channel-rpc-url "$CHANNEL_RPC_URL" --old-authority-keypair ./keypairs/admin.json --new-authority <NEW_PUBKEY> --dry-run
```

`WOULD MIGRATE` held by the old key. `OK` already migrated. `SKIP` not initialized
yet, the first deposit creates it under the new admin. `FOREIGN` held by neither
key, which aborts the run before anything is sent: resolve those via
[`reconciliation_halt_runbook.md`](reconciliation_halt_runbook.md) § "Halt reason:
stale mint authority".

**5. Migrate.** Same command without `--dry-run`. Both authorities move in one
transaction per mint, then the tool re-reads everything and exits non-zero if any
mint is still off the new authority. Non-zero means stop. Re-running is safe.

To reverse, run it again with the pubkeys swapped and the new key as
`--old-authority-keypair`.

**6. Update config.** `ADMIN_PRIVATE_KEY`, `PRIVATE_CHANNEL_ADMIN_KEYS`, and
`OPERATOR_PRIVATE_KEY` if it shares the admin key (it does in the shipped compose
files).

**7. Restart** both operators **and the write node**.
`PRIVATE_CHANNEL_ADMIN_KEYS` is read once at node startup.

**8. Verify.** The escrow operator's first reconciliation tick runs immediately at
startup. It must log `Expected channel mint authority: <NEW_PUBKEY>` (the new key,
not the old, or the env did not take), with no `MINT AUTHORITY HALT` and no row
from:

```sql
SELECT halted, reason FROM reconciliation_halt WHERE id = TRUE;
```

A halt here means a mint was missed. Return to step 4.

**9. `SetNewAdmin`,** signed by both admins, then `AllowMint` the step 1 list
signed by the new admin. This order makes the re-allow the proof that the new
escrow admin works.

**10. Resume.** Clear any halt flag
([`reconciliation_halt_runbook.md`](reconciliation_halt_runbook.md) § Recover), run
one deposit and one withdrawal, re-queue anything left in `manual_review`.

**11. Retire the old key.** Not before step 8 passed.
