use {
    super::{postgres::PostgresAccountsDB, redis::RedisAccountsDB, traits::AccountsDB},
    crate::accounts::types::StoredTransaction,
    anyhow::Result,
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
) -> Result<Vec<RpcConfirmedTransactionStatusWithSignature>> {
    match db {
        AccountsDB::Postgres(postgres_db) => {
            get_signatures_for_address_postgres(postgres_db, address, limit).await
        }
        AccountsDB::Redis(redis_db) => {
            get_signatures_for_address_redis(redis_db, address, limit).await
        }
    }
}

async fn get_signatures_for_address_postgres(
    db: &PostgresAccountsDB,
    address: &Pubkey,
    limit: usize,
) -> Result<Vec<RpcConfirmedTransactionStatusWithSignature>> {
    let pool = Arc::clone(&db.pool);
    let addr_bytes = address.to_bytes();

    // Single query: join address_signatures with transactions so we get the
    // signature, slot, AND the full transaction blob in one round-trip.
    //
    // address_signatures columns used:
    //   address   - filtered by WHERE
    //   slot      - used for ordering and returned to caller
    //   signature - returned to caller; also the JOIN key
    //
    // transactions column used:
    //   data - the serialized StoredTransaction blob (err, memo, block_time live inside)
    //
    // LEFT JOIN means: if a transaction row is somehow missing, `data` comes
    // back as NULL instead of dropping the row entirely. We handle that below.
    //
    // There is no name collision: we select `signature` and `slot` only from
    // address_signatures, and `data` only from transactions.
    let rows = match sqlx::query(
        "SELECT address_signatures.signature,
                address_signatures.slot,
                transactions.data
         FROM address_signatures
         LEFT JOIN transactions ON address_signatures.signature = transactions.signature
         WHERE address_signatures.address = $1
         ORDER BY address_signatures.slot DESC
         LIMIT $2",
    )
    .bind(&addr_bytes[..])
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
    _db: &RedisAccountsDB,
    _address: &Pubkey,
    _limit: usize,
) -> Result<Vec<RpcConfirmedTransactionStatusWithSignature>> {
    Ok(vec![])
}
