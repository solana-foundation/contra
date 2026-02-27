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
    pub const TRACE_ID: &str = "trace_id";
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
        // Ensure pgcrypto is available for gen_random_uuid()
        sqlx::query(r#"CREATE EXTENSION IF NOT EXISTS "pgcrypto""#)
            .execute(&self.pool)
            .await?;

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
                counterpart_signature TEXT,
                trace_id TEXT NOT NULL DEFAULT gen_random_uuid()::text
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

        // Idempotent migration: add trace_id to existing databases
        info!("Running trace_id migration if needed...");
        sqlx::query(
            r#"
            DO $$ BEGIN
                ALTER TABLE transactions ADD COLUMN IF NOT EXISTS trace_id TEXT;
                UPDATE transactions SET trace_id = gen_random_uuid()::text WHERE trace_id IS NULL;
                IF EXISTS (
                    SELECT 1 FROM information_schema.columns
                    WHERE table_name = 'transactions' AND column_name = 'trace_id' AND is_nullable = 'YES'
                ) THEN
                    ALTER TABLE transactions ALTER COLUMN trace_id SET NOT NULL;
                END IF;
            END $$;
            "#,
        )
        .execute(&self.pool)
        .await?;
        info!("trace_id migration complete");

        sqlx::query(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_transactions_trace_id ON transactions (trace_id)",
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

    pub async fn drop_tables(&self) -> Result<(), sqlx::Error> {
        info!("Dropping database tables...");

        // Drop tables with CASCADE to handle dependencies
        sqlx::query("DROP TABLE IF EXISTS transactions CASCADE")
            .execute(&self.pool)
            .await?;

        sqlx::query("DROP TABLE IF EXISTS indexer_state CASCADE")
            .execute(&self.pool)
            .await?;

        sqlx::query("DROP TABLE IF EXISTS mints CASCADE")
            .execute(&self.pool)
            .await?;

        // Drop sequences
        sqlx::query("DROP SEQUENCE IF EXISTS withdrawal_nonce_seq CASCADE")
            .execute(&self.pool)
            .await?;

        // Drop enum types
        sqlx::query("DROP TYPE IF EXISTS transaction_status CASCADE")
            .execute(&self.pool)
            .await?;

        sqlx::query("DROP TYPE IF EXISTS transaction_type CASCADE")
            .execute(&self.pool)
            .await?;

        info!("Database tables dropped successfully");
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
                {}, {}, {}, {}, {}, {}, {}, {}, {}, {}
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
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
            transaction_cols::TRACE_ID,
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
        .bind(&transaction.trace_id)
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
                    {}, {}, {}, {}, {}, {}, {}, {}, {}, {}
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
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
                transaction_cols::TRACE_ID,
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
            .bind(&transaction.trace_id)
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
                {}, {}, {}, {}, {}, {}
            FROM transactions
            WHERE {} = $1 AND {} = $2
            ORDER BY {} ASC
            LIMIT $3
            "#,
            transaction_cols::ID,
            transaction_cols::SIGNATURE,
            transaction_cols::TRACE_ID,
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
                {}, {}, {}, {}, {}, {}, {}
            FROM transactions
            WHERE {} = $1
            ORDER BY {} DESC
            LIMIT $2
            "#,
            transaction_cols::ID,
            transaction_cols::SIGNATURE,
            transaction_cols::TRACE_ID,
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
                {}, {}, {}, {}, {}, {}
            FROM transactions
            WHERE {} = $1 AND {} = $2
            ORDER BY {} ASC
            LIMIT $3
            FOR UPDATE SKIP LOCKED
            "#,
            transaction_cols::ID,
            transaction_cols::SIGNATURE,
            transaction_cols::TRACE_ID,
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

    /// Query escrow balances by mint for continuous reconciliation checks.
    /// Only counts **completed** transactions for both deposits and withdrawals.
    /// This provides a conservative view based on finalized database state,
    /// suitable for comparing against on-chain escrow ATA balances.
    ///
    /// Returns per-mint aggregate balances where:
    /// - `total_deposits`: sum of completed deposit amounts
    /// - `total_withdrawals`: sum of completed withdrawal amounts
    ///
    /// Expected net on-chain balance = total_deposits - total_withdrawals
    pub async fn get_escrow_balances_by_mint_internal(
        &self,
    ) -> Result<Vec<MintDbBalance>, sqlx::Error> {
        sqlx::query_as::<_, MintDbBalance>(
            r#"
            SELECT
                m.mint_address,
                m.token_program,
                COALESCE(
                    SUM(CASE WHEN t.transaction_type = 'deposit' AND t.status = 'completed' THEN t.amount ELSE 0 END),
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
