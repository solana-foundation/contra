use sqlx::{postgres::PgPoolOptions, PgPool};
use tracing::info;

use crate::{
    error::StorageError,
    storage::common::models::{
        DbMint, DbTransaction, MintDbBalance, TransactionStatus, TransactionType,
    },
    PostgresConfig,
};

mod transaction_cols {
    pub const ID: &str = "id";
    pub const SIGNATURE: &str = "signature";
    pub const SLOT: &str = "slot";
    pub const INITIATOR: &str = "initiator";
    pub const RECIPIENT: &str = "recipient";
    pub const MINT: &str = "mint";
    pub const AMOUNT: &str = "amount";
    pub const MEMO: &str = "memo";
    pub const STATUS: &str = "status";
    pub const TRANSACTION_TYPE: &str = "transaction_type";
    pub const WITHDRAWAL_NONCE: &str = "withdrawal_nonce";
    pub const CREATED_AT: &str = "created_at";
    pub const UPDATED_AT: &str = "updated_at";
    pub const PROCESSED_AT: &str = "processed_at";
    pub const COUNTERPART_SIGNATURE: &str = "counterpart_signature";
}

#[derive(Clone)]
pub struct PostgresDb {
    pool: PgPool,
}

impl PostgresDb {
    pub async fn new(config: &PostgresConfig) -> Result<Self, sqlx::Error> {
        let pool = PgPoolOptions::new()
            .max_connections(config.max_connections)
            .connect(&config.database_url)
            .await?;

        Ok(Self { pool })
    }

    pub async fn commit_transaction(
        &self,
        tx: sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<(), sqlx::Error> {
        tx.commit().await
    }

    pub async fn rollback_transaction(
        &self,
        tx: sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<(), sqlx::Error> {
        tx.rollback().await
    }

    pub async fn init_schema(&self) -> Result<(), sqlx::Error> {
        // Create enum type for transaction status
        sqlx::query(
            r#"
            DO $$ BEGIN
                CREATE TYPE transaction_status AS ENUM ('pending', 'processing', 'completed', 'failed');
            EXCEPTION
                WHEN duplicate_object THEN null;
            END $$;
            "#,
        )
        .execute(&self.pool)

        .await?;

        // Create enum type for transaction type
        sqlx::query(
            r#"
            DO $$ BEGIN
                CREATE TYPE transaction_type AS ENUM ('deposit', 'withdrawal');
            EXCEPTION
                WHEN duplicate_object THEN null;
            END $$;
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Create transactions table
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS transactions (
                id BIGSERIAL PRIMARY KEY,
                signature TEXT NOT NULL UNIQUE,
                slot BIGINT NOT NULL,
                initiator TEXT NOT NULL,
                recipient TEXT NOT NULL,
                mint TEXT NOT NULL,
                amount BIGINT NOT NULL,
                memo TEXT,
                status transaction_status NOT NULL DEFAULT 'pending',
                transaction_type transaction_type NOT NULL,
                withdrawal_nonce BIGINT,
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                processed_at TIMESTAMPTZ,
                counterpart_signature TEXT
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Create indexes for transactions
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_transactions_status ON transactions (status)")
            .execute(&self.pool)
            .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_transactions_type ON transactions (transaction_type)",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_transactions_slot ON transactions (slot)")
            .execute(&self.pool)
            .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_transactions_initiator ON transactions (initiator)",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_transactions_recipient ON transactions (recipient)",
        )
        .execute(&self.pool)
        .await?;

        // Add unique index for signatures and counterpart_signature
        sqlx::query(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_transactions_signature ON transactions (signature)",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_transactions_counterpart_signature ON transactions (counterpart_signature) WHERE counterpart_signature IS NOT NULL",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_transactions_withdrawal_nonce_unique ON transactions (withdrawal_nonce) WHERE withdrawal_nonce IS NOT NULL AND transaction_type = 'withdrawal'",
        )
        .execute(&self.pool)
        .await?;

        // Create withdrawal nonce sequence
        sqlx::query(
            r#"
            CREATE SEQUENCE IF NOT EXISTS withdrawal_nonce_seq START 0 MINVALUE 0;
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Create trigger to auto-assign withdrawal_nonce for withdrawal transactions
        sqlx::query(
            r#"
            CREATE OR REPLACE FUNCTION assign_withdrawal_nonce()
            RETURNS TRIGGER AS $$
            BEGIN
                IF NEW.transaction_type = 'withdrawal' AND NEW.withdrawal_nonce IS NULL THEN
                    NEW.withdrawal_nonce := NEXTVAL('withdrawal_nonce_seq');
                END IF;
                RETURN NEW;
            END;
            $$ LANGUAGE plpgsql;
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            DROP TRIGGER IF EXISTS trigger_assign_withdrawal_nonce ON transactions;
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TRIGGER trigger_assign_withdrawal_nonce
            BEFORE INSERT ON transactions
            FOR EACH ROW
            EXECUTE FUNCTION assign_withdrawal_nonce();
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Create indexer_state table for checkpoint tracking
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS indexer_state (
                program_type TEXT PRIMARY KEY,
                last_committed_slot BIGINT NOT NULL DEFAULT 0,
                updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_indexer_state_program ON indexer_state (program_type)",
        )
        .execute(&self.pool)
        .await?;

        // Create updated_at trigger function
        sqlx::query(
            r#"
            CREATE OR REPLACE FUNCTION update_updated_at_column()
            RETURNS TRIGGER AS $$
            BEGIN
                NEW.updated_at = NOW();
                RETURN NEW;
            END;
            $$ language 'plpgsql';
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Add triggers for updated_at
        sqlx::query(
            r#"
            DO $$
            BEGIN
                IF NOT EXISTS (SELECT 1 FROM pg_trigger WHERE tgname = 'update_transactions_updated_at') THEN
                    CREATE TRIGGER update_transactions_updated_at BEFORE UPDATE ON transactions
                    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
                END IF;

            END $$;
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Add trigger for indexer_state updated_at
        sqlx::query(
            r#"
            DO $$
            BEGIN
                IF NOT EXISTS (SELECT 1 FROM pg_trigger WHERE tgname = 'update_indexer_state_updated_at') THEN
                    CREATE TRIGGER update_indexer_state_updated_at BEFORE UPDATE ON indexer_state
                    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
                END IF;
            END $$;
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Create mints table for simple lookup
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS mints (
                mint_address TEXT PRIMARY KEY,
                decimals SMALLINT NOT NULL,
                token_program TEXT NOT NULL,
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        info!("Database schema initialized");
        Ok(())
    }

    pub async fn insert_transaction_internal(
        &self,
        transaction: &DbTransaction,
    ) -> Result<i64, sqlx::Error> {
        let existing: Option<(i64,)> = sqlx::query_as(&format!(
            "SELECT {} FROM transactions WHERE {} = $1",
            transaction_cols::ID,
            transaction_cols::SIGNATURE
        ))
        .bind(&transaction.signature)
        .fetch_optional(&self.pool)
        .await?;

        if let Some((id,)) = existing {
            return Ok(id);
        }

        let result: Option<(i64,)> = sqlx::query_as(&format!(
            r#"
            INSERT INTO transactions (
                {}, {}, {}, {}, {}, {}, {}, {}, {}
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            ON CONFLICT ({}) DO NOTHING
            RETURNING {}
            "#,
            transaction_cols::SIGNATURE,
            transaction_cols::SLOT,
            transaction_cols::INITIATOR,
            transaction_cols::RECIPIENT,
            transaction_cols::MINT,
            transaction_cols::AMOUNT,
            transaction_cols::MEMO,
            transaction_cols::TRANSACTION_TYPE,
            transaction_cols::STATUS,
            transaction_cols::SIGNATURE,
            transaction_cols::ID,
        ))
        .bind(&transaction.signature)
        .bind(transaction.slot)
        .bind(&transaction.initiator)
        .bind(&transaction.recipient)
        .bind(&transaction.mint)
        .bind(transaction.amount)
        .bind(&transaction.memo)
        .bind(transaction.transaction_type)
        .bind(transaction.status)
        .fetch_optional(&self.pool)
        .await?;

        if let Some((id,)) = result {
            return Ok(id);
        }

        // Conflict occurred, fetch existing ID
        let (id,): (i64,) = sqlx::query_as(&format!(
            "SELECT {} FROM transactions WHERE {} = $1",
            transaction_cols::ID,
            transaction_cols::SIGNATURE
        ))
        .bind(&transaction.signature)
        .fetch_one(&self.pool)
        .await?;

        Ok(id)
    }

    pub async fn insert_transactions_batch_internal(
        &self,
        transactions: &[DbTransaction],
    ) -> Result<Vec<i64>, sqlx::Error> {
        if transactions.is_empty() {
            return Ok(Vec::new());
        }

        let mut ids = Vec::with_capacity(transactions.len());

        // Use a transaction for batch insert
        let mut tx = self.pool.begin().await?;

        for transaction in transactions {
            // Check if already exists
            let existing: Option<(i64,)> = sqlx::query_as(&format!(
                "SELECT {} FROM transactions WHERE {} = $1",
                transaction_cols::ID,
                transaction_cols::SIGNATURE
            ))
            .bind(&transaction.signature)
            .fetch_optional(&mut *tx)
            .await?;

            if let Some((id,)) = existing {
                ids.push(id);
                continue;
            }

            // Insert new transaction
            let result: Option<(i64,)> = sqlx::query_as(&format!(
                r#"
                INSERT INTO transactions (
                    {}, {}, {}, {}, {}, {}, {}, {}, {}
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                ON CONFLICT ({}) DO NOTHING
                RETURNING {}
                "#,
                transaction_cols::SIGNATURE,
                transaction_cols::SLOT,
                transaction_cols::INITIATOR,
                transaction_cols::RECIPIENT,
                transaction_cols::MINT,
                transaction_cols::AMOUNT,
                transaction_cols::MEMO,
                transaction_cols::TRANSACTION_TYPE,
                transaction_cols::STATUS,
                transaction_cols::SIGNATURE,
                transaction_cols::ID,
            ))
            .bind(&transaction.signature)
            .bind(transaction.slot)
            .bind(&transaction.initiator)
            .bind(&transaction.recipient)
            .bind(&transaction.mint)
            .bind(transaction.amount)
            .bind(&transaction.memo)
            .bind(transaction.transaction_type)
            .bind(transaction.status)
            .fetch_optional(&mut *tx)
            .await?;

            if let Some((id,)) = result {
                ids.push(id);
            } else {
                // Conflict occurred, fetch existing ID
                let (id,): (i64,) = sqlx::query_as(&format!(
                    "SELECT {} FROM transactions WHERE {} = $1",
                    transaction_cols::ID,
                    transaction_cols::SIGNATURE
                ))
                .bind(&transaction.signature)
                .fetch_one(&mut *tx)
                .await?;
                ids.push(id);
            }
        }

        tx.commit().await?;
        Ok(ids)
    }

    pub async fn get_pending_withdrawals_internal(
        &self,
        transaction_type: TransactionType,
        limit: i64,
    ) -> Result<Vec<DbTransaction>, sqlx::Error> {
        sqlx::query_as::<_, DbTransaction>(&format!(
            r#"
            SELECT
                {}, {}, {}, {}, {}, {}, {}, {}, {}, {},
                {}, {}, {}, {}, {}
            FROM transactions
            WHERE {} = $1 AND {} = $2
            ORDER BY {} ASC
            LIMIT $3
            "#,
            transaction_cols::ID,
            transaction_cols::SIGNATURE,
            transaction_cols::SLOT,
            transaction_cols::INITIATOR,
            transaction_cols::RECIPIENT,
            transaction_cols::MINT,
            transaction_cols::AMOUNT,
            transaction_cols::MEMO,
            transaction_cols::TRANSACTION_TYPE,
            transaction_cols::WITHDRAWAL_NONCE,
            transaction_cols::STATUS,
            transaction_cols::CREATED_AT,
            transaction_cols::UPDATED_AT,
            transaction_cols::PROCESSED_AT,
            transaction_cols::COUNTERPART_SIGNATURE,
            // Filters
            transaction_cols::STATUS,
            transaction_cols::TRANSACTION_TYPE,
            // Ordering
            transaction_cols::ID,
        ))
        .bind(TransactionStatus::Pending)
        .bind(transaction_type)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
    }

    /// Get all transactions of a given type regardless of status
    pub async fn get_all_transactions_internal(
        &self,
        transaction_type: TransactionType,
        limit: i64,
    ) -> Result<Vec<DbTransaction>, sqlx::Error> {
        sqlx::query_as::<_, DbTransaction>(&format!(
            r#"
            SELECT
                {}, {}, {}, {}, {}, {}, {}, {}, {},
                {}, {}, {}, {}, {}, {}
            FROM transactions
            WHERE {} = $1
            ORDER BY {} DESC
            LIMIT $2
            "#,
            transaction_cols::ID,
            transaction_cols::SIGNATURE,
            transaction_cols::SLOT,
            transaction_cols::INITIATOR,
            transaction_cols::RECIPIENT,
            transaction_cols::MINT,
            transaction_cols::AMOUNT,
            transaction_cols::MEMO,
            transaction_cols::TRANSACTION_TYPE,
            transaction_cols::STATUS,
            transaction_cols::WITHDRAWAL_NONCE,
            transaction_cols::CREATED_AT,
            transaction_cols::UPDATED_AT,
            transaction_cols::PROCESSED_AT,
            transaction_cols::COUNTERPART_SIGNATURE,
            // Filter
            transaction_cols::TRANSACTION_TYPE,
            // Ordering
            transaction_cols::CREATED_AT,
        ))
        .bind(transaction_type)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
    }

    pub async fn get_committed_checkpoint_internal(
        &self,
        program_type: &str,
    ) -> Result<Option<u64>, sqlx::Error> {
        let result: Option<(i64,)> =
            sqlx::query_as("SELECT last_committed_slot FROM indexer_state WHERE program_type = $1")
                .bind(program_type)
                .fetch_optional(&self.pool)
                .await?;

        Ok(result.map(|(slot,)| slot as u64))
    }

    pub async fn update_committed_checkpoint_internal(
        &self,
        program_type: &str,
        slot: u64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            INSERT INTO indexer_state (program_type, last_committed_slot, updated_at)
            VALUES ($1, $2, NOW())
            ON CONFLICT (program_type)
            DO UPDATE SET
                last_committed_slot = EXCLUDED.last_committed_slot,
                updated_at = NOW()
            "#,
        )
        .bind(program_type)
        .bind(slot as i64)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn get_and_lock_pending_transactions_internal(
        &self,
        transaction_type: TransactionType,
        limit: i64,
    ) -> Result<Vec<DbTransaction>, sqlx::Error> {
        // Use a transaction to ensure atomicity
        let mut tx = self.pool.begin().await?;

        // Lock rows with FOR UPDATE SKIP LOCKED
        let transactions = sqlx::query_as::<_, DbTransaction>(&format!(
            r#"
            SELECT
                {}, {}, {}, {}, {}, {}, {}, {}, {}, {},
                {}, {}, {}, {}, {}
            FROM transactions
            WHERE {} = $1 AND {} = $2
            ORDER BY {} ASC
            LIMIT $3
            FOR UPDATE SKIP LOCKED
            "#,
            transaction_cols::ID,
            transaction_cols::SIGNATURE,
            transaction_cols::SLOT,
            transaction_cols::INITIATOR,
            transaction_cols::RECIPIENT,
            transaction_cols::MINT,
            transaction_cols::AMOUNT,
            transaction_cols::MEMO,
            transaction_cols::TRANSACTION_TYPE,
            transaction_cols::WITHDRAWAL_NONCE,
            transaction_cols::STATUS,
            transaction_cols::CREATED_AT,
            transaction_cols::UPDATED_AT,
            transaction_cols::PROCESSED_AT,
            transaction_cols::COUNTERPART_SIGNATURE,
            // Filters
            transaction_cols::STATUS,
            transaction_cols::TRANSACTION_TYPE,
            // Ordering (FIFO)
            transaction_cols::CREATED_AT,
        ))
        .bind(TransactionStatus::Pending)
        .bind(transaction_type)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?;

        // Update status to Processing in a single query
        if !transactions.is_empty() {
            let ids: Vec<i64> = transactions.iter().map(|txn| txn.id).collect();
            sqlx::query(&format!(
                "UPDATE transactions SET {} = $1 WHERE {} = ANY($2)",
                transaction_cols::STATUS,
                transaction_cols::ID
            ))
            .bind(TransactionStatus::Processing)
            .bind(&ids)
            .execute(&mut *tx)
            .await?;
        }

        // Commit to release locks with Processing status
        tx.commit().await?;

        Ok(transactions)
    }

    pub async fn update_transaction_status_internal(
        &self,
        transaction_id: i64,
        status: TransactionStatus,
        counterpart_signature: Option<String>,
        processed_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            UPDATE transactions
            SET
                status = $2,
                counterpart_signature = $3,
                processed_at = $4,
                updated_at = NOW()
            WHERE id = $1
            "#,
        )
        .bind(transaction_id)
        .bind(status)
        .bind(counterpart_signature)
        .bind(processed_at)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn upsert_mints_batch_internal(&self, mints: &[DbMint]) -> Result<(), StorageError> {
        if mints.is_empty() {
            return Ok(());
        }

        // Use a transaction for batch upsert
        let mut tx = self.pool.begin().await?;

        for mint in mints {
            sqlx::query(
                r#"
                INSERT INTO mints (mint_address, decimals, token_program)
                VALUES ($1, $2, $3)
                ON CONFLICT (mint_address) DO UPDATE
                SET decimals = EXCLUDED.decimals,
                    token_program = EXCLUDED.token_program
                "#,
            )
            .bind(&mint.mint_address)
            .bind(mint.decimals)
            .bind(&mint.token_program)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(())
    }

    pub async fn get_mint_internal(
        &self,
        mint_address: &str,
    ) -> Result<Option<DbMint>, StorageError> {
        let mint = sqlx::query_as::<_, DbMint>(
            r#"
            SELECT * FROM mints WHERE mint_address = $1
            "#,
        )
        .bind(mint_address)
        .fetch_optional(&self.pool)
        .await?;

        Ok(mint)
    }

    /// Return per-mint aggregate balances for startup reconciliation.
    ///
    /// For each mint known to the DB, sums:
    /// - `total_deposits`  : ALL indexed deposits (any status), because a deposit increases
    ///   the escrow ATA balance on-chain the moment it is observed — the operator's contra minting
    ///   status (`pending`/`processing`/`completed`/`failed`) does not change what is on-chain.
    /// - `total_withdrawals`: only `completed` withdrawals, because only a completed
    ///   `release_funds` call actually moves tokens out of the ATA.
    ///
    /// Mints with no transactions still appear (with totals = 0) because of the LEFT JOIN.
    pub async fn get_mint_balances_for_reconciliation_internal(
        &self,
    ) -> Result<Vec<MintDbBalance>, sqlx::Error> {
        sqlx::query_as::<_, MintDbBalance>(
            r#"
            SELECT
                m.mint_address,
                m.token_program,
                COALESCE(
                    SUM(CASE WHEN t.transaction_type = 'deposit' THEN t.amount ELSE 0 END),
                    0
                )::BIGINT AS total_deposits,
                COALESCE(
                    SUM(CASE WHEN t.transaction_type = 'withdrawal' AND t.status = 'completed' THEN t.amount ELSE 0 END),
                    0
                )::BIGINT AS total_withdrawals
            FROM mints m
            LEFT JOIN transactions t ON t.mint = m.mint_address
            GROUP BY m.mint_address, m.token_program
            "#,
        )
        .fetch_all(&self.pool)
        .await
    }

    // ── Admin API queries (read-only) ─────────────────────────────

    pub async fn get_transaction_by_signature(
        &self,
        signature: &str,
    ) -> Result<Option<DbTransaction>, sqlx::Error> {
        sqlx::query_as::<_, DbTransaction>(
            r#"
            SELECT id, signature, slot, initiator, recipient, mint, amount, memo,
                   transaction_type, withdrawal_nonce, status, created_at, updated_at,
                   processed_at, counterpart_signature
            FROM transactions WHERE signature = $1
            "#,
        )
        .bind(signature)
        .fetch_optional(&self.pool)
        .await
    }

    pub async fn get_transactions_paginated(
        &self,
        page: i64,
        per_page: i64,
        status: Option<TransactionStatus>,
        tx_type: Option<TransactionType>,
    ) -> Result<(Vec<DbTransaction>, i64), sqlx::Error> {
        let offset = (page - 1) * per_page;

        // Build count query
        let total: (i64,) =
            match (status, tx_type) {
                (Some(s), Some(t)) => sqlx::query_as(
                    "SELECT COUNT(*) FROM transactions WHERE status = $1 AND transaction_type = $2",
                )
                .bind(s)
                .bind(t)
                .fetch_one(&self.pool)
                .await?,
                (Some(s), None) => {
                    sqlx::query_as("SELECT COUNT(*) FROM transactions WHERE status = $1")
                        .bind(s)
                        .fetch_one(&self.pool)
                        .await?
                }
                (None, Some(t)) => {
                    sqlx::query_as("SELECT COUNT(*) FROM transactions WHERE transaction_type = $1")
                        .bind(t)
                        .fetch_one(&self.pool)
                        .await?
                }
                (None, None) => {
                    sqlx::query_as("SELECT COUNT(*) FROM transactions")
                        .fetch_one(&self.pool)
                        .await?
                }
            };

        let rows = match (status, tx_type) {
            (Some(s), Some(t)) => {
                sqlx::query_as::<_, DbTransaction>(
                    r#"
                    SELECT id, signature, slot, initiator, recipient, mint, amount, memo,
                           transaction_type, withdrawal_nonce, status, created_at, updated_at,
                           processed_at, counterpart_signature
                    FROM transactions
                    WHERE status = $1 AND transaction_type = $2
                    ORDER BY created_at DESC
                    LIMIT $3 OFFSET $4
                    "#,
                )
                .bind(s)
                .bind(t)
                .bind(per_page)
                .bind(offset)
                .fetch_all(&self.pool)
                .await?
            }
            (Some(s), None) => {
                sqlx::query_as::<_, DbTransaction>(
                    r#"
                    SELECT id, signature, slot, initiator, recipient, mint, amount, memo,
                           transaction_type, withdrawal_nonce, status, created_at, updated_at,
                           processed_at, counterpart_signature
                    FROM transactions
                    WHERE status = $1
                    ORDER BY created_at DESC
                    LIMIT $2 OFFSET $3
                    "#,
                )
                .bind(s)
                .bind(per_page)
                .bind(offset)
                .fetch_all(&self.pool)
                .await?
            }
            (None, Some(t)) => {
                sqlx::query_as::<_, DbTransaction>(
                    r#"
                    SELECT id, signature, slot, initiator, recipient, mint, amount, memo,
                           transaction_type, withdrawal_nonce, status, created_at, updated_at,
                           processed_at, counterpart_signature
                    FROM transactions
                    WHERE transaction_type = $1
                    ORDER BY created_at DESC
                    LIMIT $2 OFFSET $3
                    "#,
                )
                .bind(t)
                .bind(per_page)
                .bind(offset)
                .fetch_all(&self.pool)
                .await?
            }
            (None, None) => {
                sqlx::query_as::<_, DbTransaction>(
                    r#"
                    SELECT id, signature, slot, initiator, recipient, mint, amount, memo,
                           transaction_type, withdrawal_nonce, status, created_at, updated_at,
                           processed_at, counterpart_signature
                    FROM transactions
                    ORDER BY created_at DESC
                    LIMIT $1 OFFSET $2
                    "#,
                )
                .bind(per_page)
                .bind(offset)
                .fetch_all(&self.pool)
                .await?
            }
        };

        Ok((rows, total.0))
    }

    pub async fn get_status_counts(&self) -> Result<Vec<(TransactionStatus, i64)>, sqlx::Error> {
        sqlx::query_as::<_, (TransactionStatus, i64)>(
            "SELECT status, COUNT(*) FROM transactions GROUP BY status",
        )
        .fetch_all(&self.pool)
        .await
    }

    pub async fn get_type_counts(&self) -> Result<Vec<(TransactionType, i64)>, sqlx::Error> {
        sqlx::query_as::<_, (TransactionType, i64)>(
            "SELECT transaction_type, COUNT(*) FROM transactions GROUP BY transaction_type",
        )
        .fetch_all(&self.pool)
        .await
    }

    pub async fn get_throughput_window(
        &self,
        interval_secs: i64,
    ) -> Result<(i64, i64, Option<f64>), sqlx::Error> {
        // Returns (completed, failed, avg_latency_ms) for the given interval
        let row: (i64, i64, Option<f64>) = sqlx::query_as(
            r#"
            SELECT
                COALESCE(SUM(CASE WHEN status = 'completed' THEN 1 ELSE 0 END), 0)::BIGINT,
                COALESCE(SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END), 0)::BIGINT,
                AVG(
                    CASE WHEN status = 'completed' AND processed_at IS NOT NULL
                    THEN EXTRACT(EPOCH FROM (processed_at - created_at)) * 1000
                    ELSE NULL END
                )
            FROM transactions
            WHERE updated_at > NOW() - make_interval(secs => $1::double precision)
            "#,
        )
        .bind(interval_secs as f64)
        .fetch_one(&self.pool)
        .await?;

        Ok(row)
    }

    pub async fn get_recent_failures(&self, limit: i64) -> Result<Vec<DbTransaction>, sqlx::Error> {
        sqlx::query_as::<_, DbTransaction>(
            r#"
            SELECT id, signature, slot, initiator, recipient, mint, amount, memo,
                   transaction_type, withdrawal_nonce, status, created_at, updated_at,
                   processed_at, counterpart_signature
            FROM transactions
            WHERE status = 'failed'
            ORDER BY updated_at DESC
            LIMIT $1
            "#,
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
    }

    pub async fn get_stuck_transactions(
        &self,
        threshold_secs: i64,
        limit: i64,
    ) -> Result<Vec<DbTransaction>, sqlx::Error> {
        sqlx::query_as::<_, DbTransaction>(
            r#"
            SELECT id, signature, slot, initiator, recipient, mint, amount, memo,
                   transaction_type, withdrawal_nonce, status, created_at, updated_at,
                   processed_at, counterpart_signature
            FROM transactions
            WHERE status IN ('pending', 'processing')
              AND updated_at < NOW() - make_interval(secs => $1::double precision)
            ORDER BY updated_at ASC
            LIMIT $2
            "#,
        )
        .bind(threshold_secs as f64)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
    }

    pub async fn get_24h_status_counts(
        &self,
    ) -> Result<Vec<(TransactionStatus, i64)>, sqlx::Error> {
        sqlx::query_as::<_, (TransactionStatus, i64)>(
            r#"
            SELECT status, COUNT(*)
            FROM transactions
            WHERE created_at > NOW() - INTERVAL '24 hours'
            GROUP BY status
            "#,
        )
        .fetch_all(&self.pool)
        .await
    }

    pub async fn get_all_mints(&self) -> Result<Vec<DbMint>, sqlx::Error> {
        sqlx::query_as::<_, DbMint>("SELECT * FROM mints")
            .fetch_all(&self.pool)
            .await
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn close(&self) -> Result<(), sqlx::Error> {
        info!("Closing database connection pool...");
        self.pool.close().await;
        info!("Database connection pool closed");
        Ok(())
    }

    pub async fn get_completed_withdrawal_nonces_internal(
        &self,
        min_nonce: i64,
        max_nonce: i64,
    ) -> Result<Vec<i64>, sqlx::Error> {
        let nonces: Vec<(i64,)> = sqlx::query_as(
            r#"
            SELECT withdrawal_nonce FROM transactions
            WHERE transaction_type = 'withdrawal'
              AND status = 'completed'
              AND withdrawal_nonce >= $1
              AND withdrawal_nonce < $2
            ORDER BY withdrawal_nonce ASC
            "#,
        )
        .bind(min_nonce)
        .bind(max_nonce)
        .fetch_all(&self.pool)
        .await?;

        Ok(nonces.into_iter().map(|(n,)| n).collect())
    }
}
