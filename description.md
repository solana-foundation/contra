# Add `private-channel-bench-tps` transfer load testing tool

## Summary

- Adds `bench-tps/` — a standalone Rust binary that drives sustained SPL-token transfer load against a running Solana Private Channels node, measures end-to-end TPS, and surfaces bottlenecks via Prometheus/Grafana
- Adds node-side `StageMetrics` instrumentation (dedup, sigverify, sequencer, executor, settler) so pipeline stage throughput is visible during a run
- Fixes duplicate-transaction dedup rejections by embedding a memo nonce, making each transaction unique regardless of blockhash reuse
- Fixes `getSignatureStatuses` HTTP 413 errors during setup by batching ATA creation and mint-to in chunks of 200 with per-batch confirmation and retry
- Wires Prometheus scrape targets and extends the `Solana Private Channels Bench` Grafana dashboard with a pipeline waterfall, per-stage counters, and a Landed TPS panel

## What `bench-tps` does

1. **Setup** — creates N funded accounts (batched ATA + mint-to), confirming each batch before proceeding
2. **Load** — submits SPL transfers at a configurable target TPS in a background loop; each tx carries a memo nonce to prevent dedup collisions
3. **Metrics** — polls `getTransactionCount` every second to compute Landed TPS; exposes `private_channel_bench_*` Prometheus metrics scraped by the existing stack
4. **Bottleneck detection** — compares per-stage counters in Grafana (`Sent TPS → Dedup → Sigverify → Sequencer → Executor → Settler → Landed TPS`) to find the first stage where rate drops

## Node-side changes

- New `core/src/stage_metrics.rs` — `StageMetrics` trait + `PrometheusMetrics` impl with labeled counters for all five pipeline stages
- Each stage (`dedup`, `sigverify`, `sequencer`, `execution`, `settle`) calls the trait at receive/forward/drop points
- `prometheus.yml` and `docker-compose.yml` updated with bench scrape config

## Testing

- Duplicate-transaction fix validated by observing zero dedup-stage drops under sustained load
- Setup batching validated against 500-account runs that previously 413'd
