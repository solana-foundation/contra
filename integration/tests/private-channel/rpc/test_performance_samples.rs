use super::test_context::PrivateChannelContext;

pub async fn run_performance_samples_test(ctx: &PrivateChannelContext) {
    println!("\n=== Performance Samples Test ===");

    // Get all recent performance samples
    let samples = ctx
        .get_recent_performance_samples(None)
        .await
        .expect("Failed to get recent performance samples");

    println!("Retrieved {} performance samples", samples.len());

    // We should have at least one sample
    assert!(
        !samples.is_empty(),
        "Should have at least one performance sample"
    );

    let mut total_transactions = 0u64;
    let min_num_slots_threshold = 80u64; // Expect at least 80 slots per sample period

    for (idx, sample) in samples.iter().enumerate() {
        let slot = sample.slot;

        println!(
            "Sample {}: slot={}, numTransactions={}, numSlots={}, samplePeriodSecs={}, numNonVoteTransactions={}",
            idx, slot, sample.num_transactions, sample.num_slots, sample.sample_period_secs, sample.num_non_vote_transactions.unwrap_or(0)
        );

        // Check that num_slots is above threshold
        assert!(
            sample.num_slots >= min_num_slots_threshold,
            "Sample {} has num_slots={} which is below threshold of {}",
            idx,
            sample.num_slots,
            min_num_slots_threshold
        );

        // In PrivateChannel, all transactions are non-vote transactions
        assert_eq!(
            sample.num_transactions,
            sample.num_non_vote_transactions.unwrap_or(0),
            "numTransactions should equal numNonVoteTransactions in PrivateChannel"
        );

        // Check that sample_period_secs matches our config (10 seconds)
        assert_eq!(
            sample.sample_period_secs, 10,
            "samplePeriodSecs should be 10 as configured"
        );

        total_transactions += sample.num_transactions;
    }

    // Check that we processed at least some transactions across all samples
    assert!(
        total_transactions > 0,
        "Total transactions across all samples should be > 0, got {}",
        total_transactions
    );

    println!(
        "✓ Performance samples test passed: {} samples, {} total transactions",
        samples.len(),
        total_transactions
    );
}
