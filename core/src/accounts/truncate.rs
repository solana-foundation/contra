use {
    super::{postgres::PostgresAccountsDB, traits::BlockInfo},
    anyhow::{anyhow, Context, Result},
    sqlx::{Executor, PgPool, Postgres, QueryBuilder, Row},
    std::{
        collections::HashSet,
        fs,
        path::{Path, PathBuf},
        time::{Duration, SystemTime},
    },
    tracing::warn,
};

const FIRST_AVAILABLE_BLOCK_KEY: &str = "first_available_block";
const SLOT_COLUMN_CANDIDATES: &[&str] = &["slot", "updated_slot", "last_updated_slot"];

#[derive(Debug, Clone)]
pub struct TruncateOptions {
    pub keep_slots: u64,
    pub max_backup_age: Duration,
    pub pg_dump_path: Option<PathBuf>,
    pub batch_size: usize,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Default)]
pub struct TruncateReport {
    pub latest_slot: Option<u64>,
    pub truncate_before_slot: Option<u64>,
    pub blocks_deleted: u64,
    pub transactions_deleted: u64,
    pub account_history_rows_deleted: u64,
    pub account_history_tables_touched: Vec<String>,
    pub backup_check: BackupCheckResult,
    pub first_available_block: Option<u64>,
    pub vacuumed_tables: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct BackupCheckResult {
    pub wal_archive_ok: bool,
    pub wal_archive_reason: String,
    pub pg_dump_ok: bool,
    pub pg_dump_reason: String,
}

impl BackupCheckResult {
    pub fn has_valid_backup(&self) -> bool {
        self.wal_archive_ok || self.pg_dump_ok
    }

    fn skipped() -> Self {
        Self {
            wal_archive_ok: false,
            wal_archive_reason: "Skipped: no rows eligible for truncation".to_string(),
            pg_dump_ok: false,
            pg_dump_reason: "Skipped: no rows eligible for truncation".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
struct AccountHistoryTable {
    name: String,
    slot_column: String,
}

pub async fn truncate_slots(
    db: &PostgresAccountsDB,
    options: &TruncateOptions,
) -> Result<TruncateReport> {
    if options.keep_slots == 0 {
        return Err(anyhow!("keep_slots must be greater than 0"));
    }
    if options.batch_size == 0 {
        return Err(anyhow!("batch_size must be greater than 0"));
    }

    let pool = db.pool.clone();
    let latest_slot = query_latest_slot(pool.as_ref()).await?;

    let Some(latest_slot) = latest_slot else {
        return Ok(TruncateReport {
            latest_slot: None,
            truncate_before_slot: None,
            backup_check: BackupCheckResult::skipped(),
            first_available_block: None,
            ..TruncateReport::default()
        });
    };

    let truncate_before_slot = compute_truncate_before_slot(latest_slot, options.keep_slots);
    let account_history_tables = discover_account_history_tables(pool.as_ref()).await?;
    let account_history_rows_to_delete = count_account_history_rows_before(
        pool.as_ref(),
        &account_history_tables,
        truncate_before_slot,
    )
    .await?;
    let blocks_to_delete = count_blocks_before(pool.as_ref(), truncate_before_slot).await?;

    let should_truncate = blocks_to_delete > 0 || account_history_rows_to_delete > 0;

    let mut report = TruncateReport {
        latest_slot: Some(latest_slot),
        truncate_before_slot: Some(truncate_before_slot),
        account_history_tables_touched: account_history_tables
            .iter()
            .map(|t| t.name.clone())
            .collect(),
        first_available_block: query_first_available_slot(pool.as_ref()).await?,
        ..TruncateReport::default()
    };

    if !should_truncate {
        report.backup_check = BackupCheckResult::skipped();
        return Ok(report);
    }

    let backup_check = verify_backup_readiness(
        pool.as_ref(),
        options.pg_dump_path.as_deref(),
        options.max_backup_age,
    )
    .await;
    report.backup_check = backup_check;

    if !report.backup_check.has_valid_backup() {
        return Err(anyhow!(
            "Backup verification failed. WAL: {}. pg_dump: {}",
            report.backup_check.wal_archive_reason,
            report.backup_check.pg_dump_reason
        ));
    }

    if options.dry_run {
        let (_, tx_count) = process_block_batches(
            pool.as_ref(),
            truncate_before_slot,
            options.batch_size,
            true,
        )
        .await?;
        report.blocks_deleted = blocks_to_delete;
        report.transactions_deleted = tx_count;
        report.account_history_rows_deleted = account_history_rows_to_delete;
        return Ok(report);
    }

    let (blocks_deleted, transactions_deleted) = process_block_batches(
        pool.as_ref(),
        truncate_before_slot,
        options.batch_size,
        false,
    )
    .await?;
    report.blocks_deleted = blocks_deleted;
    report.transactions_deleted = transactions_deleted;

    let account_history_rows_deleted =
        truncate_account_history_rows(pool.as_ref(), &account_history_tables, truncate_before_slot)
            .await?;
    report.account_history_rows_deleted = account_history_rows_deleted;

    report.first_available_block = set_first_available_block_metadata(
        pool.as_ref(),
        query_first_available_slot(pool.as_ref()).await?,
    )
    .await?;

    let mut vacuum_targets = Vec::new();
    if blocks_deleted > 0 || transactions_deleted > 0 {
        vacuum_targets.push("blocks".to_string());
        vacuum_targets.push("transactions".to_string());
    }
    if account_history_rows_deleted > 0 {
        for table in &account_history_tables {
            vacuum_targets.push(table.name.clone());
        }
    }
    vacuum_targets.sort();
    vacuum_targets.dedup();

    run_vacuum(pool.as_ref(), &vacuum_targets).await?;
    report.vacuumed_tables = vacuum_targets;

    Ok(report)
}

async fn process_block_batches(
    pool: &PgPool,
    truncate_before_slot: u64,
    batch_size: usize,
    dry_run: bool,
) -> Result<(u64, u64)> {
    let mut total_blocks = 0_u64;
    let mut total_transactions = 0_u64;
    let mut last_processed_slot: Option<i64> = None;

    loop {
        let rows = match last_processed_slot {
            Some(last_slot) => {
                sqlx::query(
                    "SELECT slot, data
                     FROM blocks
                     WHERE slot < $1
                       AND slot > $2
                     ORDER BY slot ASC
                     LIMIT $3",
                )
                .bind(truncate_before_slot as i64)
                .bind(last_slot)
                .bind(batch_size as i64)
                .fetch_all(pool)
                .await
            }
            None => {
                sqlx::query(
                    "SELECT slot, data
                     FROM blocks
                     WHERE slot < $1
                     ORDER BY slot ASC
                     LIMIT $2",
                )
                .bind(truncate_before_slot as i64)
                .bind(batch_size as i64)
                .fetch_all(pool)
                .await
            }
        }
        .context("Failed to fetch blocks for truncation")?;

        if rows.is_empty() {
            break;
        }

        let mut slots = Vec::with_capacity(rows.len());
        let mut signatures = HashSet::new();

        for row in rows {
            let slot: i64 = row.get("slot");
            let data: Vec<u8> = row.get("data");
            let block: BlockInfo = bincode::deserialize(&data)
                .with_context(|| format!("Failed to deserialize block at slot {}", slot))?;

            slots.push(slot);
            for signature in block.transaction_signatures {
                signatures.insert(signature.as_ref().to_vec());
            }
        }

        if let Some(slot) = slots.last().copied() {
            last_processed_slot = Some(slot);
        }

        total_blocks += slots.len() as u64;
        total_transactions += signatures.len() as u64;

        if dry_run {
            continue;
        }

        let mut tx = pool
            .begin()
            .await
            .context("Failed to begin truncation transaction")?;

        if !signatures.is_empty() {
            let mut builder: QueryBuilder<'_, Postgres> =
                QueryBuilder::new("DELETE FROM transactions WHERE signature IN (");
            let mut separated = builder.separated(", ");
            for signature in signatures {
                separated.push_bind(signature);
            }
            separated.push_unseparated(")");
            builder
                .build()
                .execute(&mut *tx)
                .await
                .context("Failed to delete old transactions")?;
        }

        let mut builder: QueryBuilder<'_, Postgres> =
            QueryBuilder::new("DELETE FROM blocks WHERE slot IN (");
        let mut separated = builder.separated(", ");
        for slot in slots {
            separated.push_bind(slot);
        }
        separated.push_unseparated(")");
        builder
            .build()
            .execute(&mut *tx)
            .await
            .context("Failed to delete old blocks")?;

        tx.commit()
            .await
            .context("Failed to commit truncation batch transaction")?;
    }

    Ok((total_blocks, total_transactions))
}

async fn discover_account_history_tables(pool: &PgPool) -> Result<Vec<AccountHistoryTable>> {
    let rows = sqlx::query(
        "SELECT table_name
         FROM information_schema.tables
         WHERE table_schema = 'public'
           AND table_name IN ('account_history', 'accounts_history')",
    )
    .fetch_all(pool)
    .await
    .context("Failed to discover account history tables")?;

    let mut tables = Vec::new();

    for row in rows {
        let table_name: String = row.get("table_name");
        let columns = sqlx::query(
            "SELECT column_name
             FROM information_schema.columns
             WHERE table_schema = 'public'
               AND table_name = $1",
        )
        .bind(&table_name)
        .fetch_all(pool)
        .await
        .with_context(|| format!("Failed to inspect columns for table {}", table_name))?;

        let column_set: HashSet<String> = columns
            .into_iter()
            .map(|column_row| column_row.get::<String, _>("column_name"))
            .collect();

        let maybe_slot_column = SLOT_COLUMN_CANDIDATES
            .iter()
            .find(|candidate| column_set.contains(**candidate))
            .map(|s| (*s).to_string());

        match maybe_slot_column {
            Some(slot_column) => tables.push(AccountHistoryTable {
                name: table_name,
                slot_column,
            }),
            None => warn!(
                "Skipping account history table '{}' because no slot column was found among {:?}",
                table_name, SLOT_COLUMN_CANDIDATES
            ),
        }
    }

    Ok(tables)
}

async fn count_account_history_rows_before(
    pool: &PgPool,
    tables: &[AccountHistoryTable],
    truncate_before_slot: u64,
) -> Result<u64> {
    let mut total_rows = 0_u64;

    for table in tables {
        let sql = format!(
            "SELECT COUNT(*) FROM {} WHERE {} < $1",
            quote_ident(&table.name),
            quote_ident(&table.slot_column)
        );
        let rows = sqlx::query_scalar::<_, i64>(&sql)
            .bind(truncate_before_slot as i64)
            .fetch_one(pool)
            .await
            .with_context(|| format!("Failed counting rows for {}", table.name))?;
        total_rows += rows as u64;
    }

    Ok(total_rows)
}

async fn truncate_account_history_rows(
    pool: &PgPool,
    tables: &[AccountHistoryTable],
    truncate_before_slot: u64,
) -> Result<u64> {
    let mut total_deleted = 0_u64;

    for table in tables {
        let sql = format!(
            "DELETE FROM {} WHERE {} < $1",
            quote_ident(&table.name),
            quote_ident(&table.slot_column)
        );
        let result = sqlx::query(&sql)
            .bind(truncate_before_slot as i64)
            .execute(pool)
            .await
            .with_context(|| format!("Failed deleting old rows from {}", table.name))?;
        total_deleted += result.rows_affected();
    }

    Ok(total_deleted)
}

async fn set_first_available_block_metadata(
    pool: &PgPool,
    slot: Option<u64>,
) -> Result<Option<u64>> {
    match slot {
        Some(slot) => {
            sqlx::query(
                "INSERT INTO metadata (key, value) VALUES ($1, $2)
                 ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
            )
            .bind(FIRST_AVAILABLE_BLOCK_KEY)
            .bind(slot.to_le_bytes().to_vec())
            .execute(pool)
            .await
            .context("Failed to update first_available_block metadata")?;
            Ok(Some(slot))
        }
        None => {
            sqlx::query("DELETE FROM metadata WHERE key = $1")
                .bind(FIRST_AVAILABLE_BLOCK_KEY)
                .execute(pool)
                .await
                .context("Failed to clear first_available_block metadata")?;
            Ok(None)
        }
    }
}

async fn run_vacuum(pool: &PgPool, table_names: &[String]) -> Result<()> {
    for table_name in table_names {
        let sql = format!("VACUUM (ANALYZE) {}", quote_ident(table_name));
        pool.execute(sql.as_str())
            .await
            .with_context(|| format!("Failed to VACUUM table {}", table_name))?;
    }
    Ok(())
}

async fn verify_backup_readiness(
    pool: &PgPool,
    pg_dump_path: Option<&Path>,
    max_backup_age: Duration,
) -> BackupCheckResult {
    let (wal_archive_ok, wal_archive_reason) =
        match check_wal_archive_recency(pool, max_backup_age).await {
            Ok(message) => (true, message),
            Err(e) => (false, e.to_string()),
        };

    let (pg_dump_ok, pg_dump_reason) = check_pg_dump_recency(pg_dump_path, max_backup_age);

    BackupCheckResult {
        wal_archive_ok,
        wal_archive_reason,
        pg_dump_ok,
        pg_dump_reason,
    }
}

async fn check_wal_archive_recency(pool: &PgPool, max_backup_age: Duration) -> Result<String> {
    let archive_mode = sqlx::query_scalar::<_, String>(
        "SELECT setting FROM pg_settings WHERE name = 'archive_mode'",
    )
    .fetch_one(pool)
    .await
    .context("Unable to read archive_mode from pg_settings")?;

    if archive_mode != "on" && archive_mode != "always" {
        return Err(anyhow!(
            "archive_mode is '{}' (expected 'on' or 'always')",
            archive_mode
        ));
    }

    let archive_command = sqlx::query_scalar::<_, String>(
        "SELECT setting FROM pg_settings WHERE name = 'archive_command'",
    )
    .fetch_one(pool)
    .await
    .context("Unable to read archive_command from pg_settings")?;

    if is_noop_archive_command(&archive_command) {
        return Err(anyhow!(
            "archive_command '{}' is a no-op and does not provide recoverable WAL archives",
            archive_command
        ));
    }

    let age_seconds = sqlx::query_scalar::<_, Option<f64>>(
        "SELECT EXTRACT(EPOCH FROM (NOW() - last_archived_time)) FROM pg_stat_archiver",
    )
    .fetch_one(pool)
    .await
    .context("Unable to read last_archived_time from pg_stat_archiver")?;

    let age_seconds = age_seconds.context("No archived WAL segment found in pg_stat_archiver")?;
    if age_seconds > max_backup_age.as_secs_f64() {
        return Err(anyhow!(
            "Latest archived WAL segment is {:.0} seconds old (max allowed: {:.0})",
            age_seconds,
            max_backup_age.as_secs_f64()
        ));
    }

    Ok(format!(
        "WAL archiving healthy; latest archived segment age {:.0} seconds",
        age_seconds
    ))
}

fn check_pg_dump_recency(pg_dump_path: Option<&Path>, max_backup_age: Duration) -> (bool, String) {
    let Some(path) = pg_dump_path else {
        return (false, "No pg_dump path supplied".to_string());
    };

    let latest_backup = match latest_backup_time(path) {
        Ok(value) => value,
        Err(e) => return (false, e.to_string()),
    };

    let age = match SystemTime::now().duration_since(latest_backup.1) {
        Ok(duration) => duration,
        Err(_) => Duration::from_secs(0),
    };

    if age > max_backup_age {
        return (
            false,
            format!(
                "Latest pg_dump artifact '{}' is {} seconds old (max allowed: {})",
                latest_backup.0.display(),
                age.as_secs(),
                max_backup_age.as_secs()
            ),
        );
    }

    (
        true,
        format!(
            "Recent pg_dump artifact '{}' found (age {} seconds)",
            latest_backup.0.display(),
            age.as_secs()
        ),
    )
}

fn latest_backup_time(path: &Path) -> Result<(PathBuf, SystemTime)> {
    if !path.exists() {
        return Err(anyhow!("pg_dump path '{}' does not exist", path.display()));
    }

    if path.is_file() {
        let modified = fs::metadata(path)
            .with_context(|| format!("Unable to read metadata for '{}'", path.display()))?
            .modified()
            .with_context(|| format!("Unable to read modified time for '{}'", path.display()))?;
        return Ok((path.to_path_buf(), modified));
    }

    if !path.is_dir() {
        return Err(anyhow!(
            "pg_dump path '{}' is neither a file nor a directory",
            path.display()
        ));
    }

    let mut latest: Option<(PathBuf, SystemTime)> = None;

    for entry in fs::read_dir(path)
        .with_context(|| format!("Unable to read pg_dump directory '{}'", path.display()))?
    {
        let entry =
            entry.with_context(|| format!("Unable to read entry in '{}'", path.display()))?;
        let entry_path = entry.path();

        if !entry_path.is_file() {
            continue;
        }

        let modified = entry
            .metadata()
            .with_context(|| format!("Unable to read metadata for '{}'", entry_path.display()))?
            .modified()
            .with_context(|| {
                format!(
                    "Unable to read modified time for '{}'",
                    entry_path.display()
                )
            })?;

        let should_replace = latest
            .as_ref()
            .map(|(_, ts)| modified > *ts)
            .unwrap_or(true);
        if should_replace {
            latest = Some((entry_path, modified));
        }
    }

    latest.context(format!(
        "No backup files found in pg_dump directory '{}'",
        path.display()
    ))
}

async fn count_blocks_before(pool: &PgPool, truncate_before_slot: u64) -> Result<u64> {
    let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM blocks WHERE slot < $1")
        .bind(truncate_before_slot as i64)
        .fetch_one(pool)
        .await
        .context("Failed to count old blocks")?;
    Ok(count as u64)
}

fn compute_truncate_before_slot(latest_slot: u64, keep_slots: u64) -> u64 {
    latest_slot.saturating_sub(keep_slots.saturating_sub(1))
}

async fn query_latest_slot(pool: &PgPool) -> Result<Option<u64>> {
    let latest_slot = sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(slot) FROM blocks")
        .fetch_one(pool)
        .await
        .context("Failed to query latest slot")?;
    Ok(latest_slot.map(|slot| slot as u64))
}

async fn query_first_available_slot(pool: &PgPool) -> Result<Option<u64>> {
    let first_available_slot = sqlx::query_scalar::<_, Option<i64>>("SELECT MIN(slot) FROM blocks")
        .fetch_one(pool)
        .await
        .context("Failed to query first available slot")?;
    Ok(first_available_slot.map(|slot| slot as u64))
}

fn quote_ident(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn is_noop_archive_command(command: &str) -> bool {
    let normalized = command.trim().trim_matches('\'').trim_matches('"');
    normalized.is_empty() || normalized == "/bin/true" || normalized == "true" || normalized == ":"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_cutoff_slot_keeps_recent_window() {
        assert_eq!(compute_truncate_before_slot(100, 10), 91);
        assert_eq!(compute_truncate_before_slot(10, 1), 10);
        assert_eq!(compute_truncate_before_slot(8, 16), 0);
    }

    #[test]
    fn noop_archive_command_detection_is_strict() {
        assert!(is_noop_archive_command("/bin/true"));
        assert!(is_noop_archive_command(" true "));
        assert!(is_noop_archive_command("':'"));
        assert!(!is_noop_archive_command("cp %p /backups/%f"));
    }
}
