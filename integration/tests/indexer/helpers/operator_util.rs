#![allow(dead_code)]

use super::db;
use super::test_types::WAIT_TIMEOUT_SECS;

pub async fn wait_for_operator_completion(
    pool: &sqlx::PgPool,
    expected_count: usize,
    operation_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let start = std::time::Instant::now();
    let mut last_logged = 0u64;
    let mut ready = false;

    while start.elapsed().as_secs() < WAIT_TIMEOUT_SECS {
        let completed_count = db::count_transactions_by_status(pool, "completed").await?;

        if completed_count >= expected_count as i64 {
            println!(
                "✓ Reached target: {}/{} transactions completed",
                completed_count, expected_count
            );
            ready = true;
            break;
        }

        // Log progress every 5 seconds
        let elapsed = start.elapsed().as_secs();
        if elapsed - last_logged >= 5 {
            println!(
                "  Progress: {}/{} completed ({:.1}%) - elapsed: {}s",
                completed_count,
                expected_count,
                (completed_count as f64 / expected_count as f64) * 100.0,
                elapsed
            );
            last_logged = elapsed;
        }

        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    }

    if !ready {
        let final_count = db::count_transactions_by_status(pool, "completed").await?;
        println!(
            "✗ Timeout: only {}/{} transactions completed",
            final_count, expected_count
        );
    }

    assert!(
        ready,
        "Operator did not process all {} within timeout",
        operation_name
    );

    Ok(())
}

pub async fn wait_for_transaction_completion(
    pool: &sqlx::PgPool,
    signature: &str,
    timeout_secs: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let start = std::time::Instant::now();

    while start.elapsed().as_secs() < timeout_secs {
        if let Some(tx) = db::get_transaction(pool, signature).await? {
            if tx.status == "completed" {
                return Ok(());
            }
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }

    Err(format!(
        "Transaction {} did not complete within {}s",
        signature, timeout_secs
    )
    .into())
}
