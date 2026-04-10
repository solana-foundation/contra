use {
    super::{postgres::PostgresAccountsDB, redis::RedisAccountsDB, traits::AccountsDB},
    crate::accounts::types::StoredTransaction,
    anyhow::Result,
    redis::AsyncCommands,
    solana_rpc_client_api::response::RpcConfirmedTransactionStatusWithSignature,
    solana_sdk::{pubkey::Pubkey, signature::Signature},
    solana_transaction_status::{extract_memos::extract_and_fmt_memos, TransactionWithStatusMeta},
    solana_transaction_status_client_types::TransactionConfirmationStatus,
    sqlx::Row,
    std::sync::Arc,
};

pub async fn get_signatures_for_address(
    db: &AccountsDB,
    address: &Pubkey,
    limit: usize,
    before: Option<&Signature>,
    until: Option<&Signature>,
) -> Result<Vec<RpcConfirmedTransactionStatusWithSignature>> {
    match db {
        AccountsDB::Postgres(postgres_db) => {
            get_signatures_for_address_postgres(postgres_db, address, limit, before, until).await
        }
        AccountsDB::Redis(redis_db) => {
            get_signatures_for_address_redis(redis_db, address, limit, before, until).await
        }
    }
}

async fn get_signatures_for_address_postgres(
    db: &PostgresAccountsDB,
    address: &Pubkey,
    limit: usize,
    before: Option<&Signature>,
    until: Option<&Signature>,
) -> Result<Vec<RpcConfirmedTransactionStatusWithSignature>> {
    let pool = Arc::clone(&db.pool);
    let addr_bytes = address.to_bytes();
    let before_bytes: Option<&[u8]> = before.map(|s| s.as_ref());
    let until_bytes: Option<&[u8]> = until.map(|s| s.as_ref());

    // Single query: join address_signatures with transactions so we get the
    // signature, slot, AND the full transaction blob in one round-trip.
    //
    // Results are ordered newest-first: ORDER BY slot DESC, signature DESC.
    // The signature tiebreaker makes the sort stable when multiple transactions
    // land in the same slot.
    //
    // ── Pagination cursors ($2 = before, $3 = until) ──────────────────────
    //
    // Both cursors are optional transaction signatures used to paginate.
    // When not provided ($2/$3 IS NULL), the condition short-circuits to TRUE
    // and no filtering is applied.
    //
    // To compare a cursor against other rows we resolve it to a (slot, signature)
    // position via a subquery. The same signature appears in address_signatures
    // once per account key it touched, but all those rows share the same slot,
    // so LIMIT 1 safely picks any one of them.
    //
    // We use PostgreSQL row comparison — treating (slot, signature) as a single
    // composite value — which mirrors the ORDER BY exactly and handles same-slot
    // tiebreaking correctly.
    //
    // before (exclusive, $2):
    //   Return transactions that occurred BEFORE this one. In newest-first order,
    //   "before" means further down the list — a smaller (slot, sig) value.
    //   Condition: (slot, sig) < (before_slot, before_sig)
    //   The cursor row itself is excluded (strict less-than).
    //
    // until (inclusive, $3):
    //   Return transactions down to and INCLUDING this one. "Until" is the oldest
    //   result we return — the last row in the result set.
    //   Condition: (slot, sig) >= (until_slot, until_sig)
    //   The cursor row itself is included (greater-than-or-equal).
    //
    // LEFT JOIN: if a transaction row is missing, data comes back as NULL
    // instead of dropping the row. We treat that as data corruption below.
    let rows = match sqlx::query(
        "SELECT address_signatures.signature,
                address_signatures.slot,
                transactions.data
         FROM address_signatures
         LEFT JOIN transactions ON address_signatures.signature = transactions.signature
         WHERE address_signatures.address = $1
           AND ($2::bytea IS NULL
                OR (address_signatures.slot, address_signatures.signature) < (
                  SELECT slot, signature FROM address_signatures WHERE signature = $2 LIMIT 1
                ))
           AND ($3::bytea IS NULL
                OR (address_signatures.slot, address_signatures.signature) >= (
                  SELECT slot, signature FROM address_signatures WHERE signature = $3 LIMIT 1
                ))
         ORDER BY address_signatures.slot DESC, address_signatures.signature DESC
         LIMIT $4",
    )
    .bind(&addr_bytes[..])
    .bind(before_bytes)
    .bind(until_bytes)
    .bind(limit as i64)
    .fetch_all(pool.as_ref())
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            return Err(anyhow::anyhow!(
                "Failed to query signatures for {}: {}",
                address,
                e
            ));
        }
    };

    let mut results = Vec::with_capacity(rows.len());
    for row in rows {
        // These three columns come from the single JOIN query above.
        let sig_bytes: Vec<u8> = row.get("signature");
        let slot: i64 = row.get("slot");
        // NULL when the transaction row is missing (should never happen since
        // address_signatures and transactions are written in the same atomic tx).
        let tx_data: Option<Vec<u8>> = row.get("data");

        let signature = match Signature::try_from(sig_bytes.as_slice()) {
            Ok(s) => s,
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "Failed to deserialize signature from address_signatures: {}",
                    e
                ));
            }
        };

        // address_signatures and transactions are written in the same atomic
        // DB transaction, so a missing transaction row is data corruption.
        let tx_data = match tx_data {
            Some(data) => data,
            None => {
                return Err(anyhow::anyhow!(
                    "Transaction data missing for signature {} — address_signatures index is inconsistent",
                    signature
                ));
            }
        };

        let stored_tx = match bincode::deserialize::<StoredTransaction>(&tx_data) {
            Ok(tx) => tx,
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "Failed to deserialize transaction {}: {}",
                    signature,
                    e
                ));
            }
        };

        let err = stored_tx.meta.err.clone();
        let memo = match stored_tx.transaction_with_status_meta() {
            TransactionWithStatusMeta::Complete(versioned) => extract_and_fmt_memos(&versioned),
            _ => None,
        };

        results.push(RpcConfirmedTransactionStatusWithSignature {
            signature: signature.to_string(),
            slot: slot as u64,
            err,
            memo,
            block_time: Some(stored_tx.block_time),
            confirmation_status: Some(TransactionConfirmationStatus::Finalized),
        });
    }

    Ok(results)
}

async fn get_signatures_for_address_redis(
    db: &RedisAccountsDB,
    address: &Pubkey,
    limit: usize,
    before: Option<&Signature>,
    until: Option<&Signature>,
) -> Result<Vec<RpcConfirmedTransactionStatusWithSignature>> {
    // before/until cursor pagination is not yet implemented for Redis.
    // All current callers (mint.rs idempotency check, integration tests) pass
    // None for both cursors. Implementing cursors requires same-slot tiebreaking
    // which is non-trivial with a score-only sorted set index — left as a TODO.
    if before.is_some() || until.is_some() {
        return Err(anyhow::anyhow!(
            "before/until cursors are not yet supported by the Redis backend"
        ));
    }

    let mut conn = db.connection.clone();
    let key = format!("addr_sigs:{}", address);

    // ZREVRANGEBYSCORE returns members with the highest score (most recent slot)
    // first, matching the Postgres ORDER BY slot DESC behaviour.
    let sig_strings: Vec<String> = redis::cmd("ZREVRANGEBYSCORE")
        .arg(&key)
        .arg("+inf")
        .arg("-inf")
        .arg("LIMIT")
        .arg(0i64)
        .arg(limit as i64)
        .query_async(&mut conn)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to query addr_sigs for {}: {}", address, e))?;

    if sig_strings.is_empty() {
        return Ok(vec![]);
    }

    // Fetch all transaction blobs in a single MGET to avoid N round trips.
    let tx_keys: Vec<String> = sig_strings.iter().map(|s| format!("tx:{}", s)).collect();
    let tx_blobs: Vec<Option<Vec<u8>>> = conn
        .mget(&tx_keys)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to MGET transactions for {}: {}", address, e))?;

    let mut results = Vec::with_capacity(sig_strings.len());
    for (sig_str, blob) in sig_strings.iter().zip(tx_blobs) {
        let data = match blob {
            Some(d) => d,
            // addr_sigs and tx:{sig} are written in the same pipeline MULTI/EXEC,
            // so a missing transaction blob indicates data corruption.
            None => {
                return Err(anyhow::anyhow!(
                    "Transaction data missing for signature {} — addr_sigs index is inconsistent",
                    sig_str
                ))
            }
        };

        let stored_tx = bincode::deserialize::<StoredTransaction>(&data)
            .map_err(|e| anyhow::anyhow!("Failed to deserialize transaction {}: {}", sig_str, e))?;

        let err = stored_tx.meta.err.clone();
        let memo = match stored_tx.transaction_with_status_meta() {
            TransactionWithStatusMeta::Complete(versioned) => extract_and_fmt_memos(&versioned),
            _ => None,
        };

        results.push(RpcConfirmedTransactionStatusWithSignature {
            signature: sig_str.clone(),
            slot: stored_tx.slot,
            err,
            memo,
            block_time: Some(stored_tx.block_time),
            confirmation_status: Some(TransactionConfirmationStatus::Finalized),
        });
    }

    Ok(results)
}
