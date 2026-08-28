# Configuration & Operations

Reference for configuring, tuning, and operating Solana Private Channels services.

**Note:** When running via Docker Compose, all configuration is set through environment variables in `.env.devnet` (or your environment file). The CLI flags listed below are their equivalent for running binaries directly. You do not need to modify Dockerfiles or container commands — just update your env file and restart the service.

---

## Configuration Reference

### Write Node (`private-channel-node --mode write`)

**Source**: [`core/src/bin/node.rs`](../core/src/bin/node.rs)

| Flag | Env Var | Default | Description |
|------|---------|---------|-------------|
| `--mode` | `PRIVATE_CHANNEL_MODE` | — | Node mode: `read`, `write`, or `aio` (all-in-one). Required, so a dropped variable fails startup instead of starting a read deployment as a writer |
| `--port` | `PRIVATE_CHANNEL_PORT` | `8899` | RPC listen port |
| `--sigverify-workers` | `PRIVATE_CHANNEL_SIGVERIFY_WORKERS` | `4` | Parallel signature verification threads |
| `--sigverify-queue-size` | `PRIVATE_CHANNEL_SIGVERIFY_QUEUE_SIZE` | `1000` | Bounded queue between dedup and sigverify |
| `--ingress-queue-capacity` | `PRIVATE_CHANNEL_INGRESS_QUEUE_CAPACITY` | `10000` | Bounded RPC→dedup queue; a full queue sheds (`sendTransaction` returns `-32003`, retryable) and increments `rpc_ingress_shed_total` |
| `--sequencer-queue-capacity` | `PRIVATE_CHANNEL_SEQUENCER_QUEUE_CAPACITY` | `1000` | Bounded sigverify→sequencer queue; a full queue applies upstream backpressure |
| `--execution-results-capacity` | `PRIVATE_CHANNEL_EXECUTION_RESULTS_CAPACITY` | `1000` | Bounded executor→settler queue; a full queue applies upstream backpressure. The settler also stops draining once a tick's buffered account bytes reach an internal budget, so the queue is bounded by bytes as well as depth |
| `--max-tx-per-batch` | `PRIVATE_CHANNEL_MAX_TX_PER_BATCH` | `64` | Max transactions per sequencer batch |
| `--max-connections` | `PRIVATE_CHANNEL_MAX_CONNECTIONS` | `100` | Max concurrent RPC connections |
| `--blocktime-ms` | `PRIVATE_CHANNEL_BLOCKTIME_MS` | `100` | Settlement interval (ms) |
| `--transaction-expiration-ms` | `PRIVATE_CHANNEL_TRANSACTION_EXPIRATION_MS` | `15000` | Transaction lifetime before dedup eviction |
| `--admin-keys` | `PRIVATE_CHANNEL_ADMIN_KEYS` | — | Comma-separated base58 channel admin pubkeys, gating SPL `InitializeMint`. Not the escrow `Instance.admin`, which is a separate offline key |
| `--accountsdb-connection-url` | `PRIVATE_CHANNEL_ACCOUNTSDB_CONNECTION_URL` | — | PostgreSQL connection string. Must be PostgreSQL: Redis is a cache, never the accounts database |
| `--redis-cache-url` | `PRIVATE_CHANNEL_REDIS_CACHE_URL` | — | Optional Redis cache in front of the read path. Misses resolve against PostgreSQL |
| `--log-level` | `PRIVATE_CHANNEL_LOG_LEVEL` | `info` | Log level: `trace`, `debug`, `info`, `warn`, `error` |
| `--json-logs` | `PRIVATE_CHANNEL_JSON_LOGS` | `false` | Structured JSON log output |
| `--perf-sample-period-secs` | `PRIVATE_CHANNEL_PERF_SAMPLE_PERIOD_SECS` | `60` | Performance metrics sampling interval |
| `--metrics` | `PRIVATE_CHANNEL_METRICS` | `false` | Enable Prometheus stage metrics server |
| — | `PRIVATE_CHANNEL_METRICS_PORT` | `9090` | Port for the stage metrics server |

**Startup validation:** The node rejects a missing `--mode`, `blocktime_ms == 0`, `transaction_expiration_ms < blocktime_ms`, and any write-pipeline queue capacity of `0` (which would panic the channel constructors) to prevent misconfiguration.

**Blockhash window and the operators:** the blockhash validity window (`transaction_expiration_ms / blocktime_ms`) bounds how much history an operator must see retained before it may call a channel signature dead. Operators read it off the node itself, seeding it at startup and re-reading it at each such verdict, so changing either setting needs no operator restart and no operator config. An operator that cannot read it warns and falls back to 150 at startup, then reports the verdict as uncertain rather than dead if the endpoint is still unreadable when one is needed.

**Tuning the queue capacities:** these aren't machine-spec values. Each slot holds one transaction (a few KB), so even the `10000` ingress queue is only ~tens of MB — RAM is never the limit. Size them by traffic, not hardware: a larger ingress queue absorbs bigger bursts but adds latency when backed up. Tune by watching `rpc_ingress_shed_total` — raise `--ingress-queue-capacity` if real bursts are being shed, lower it if latency under load is too high.

### Read Node (`private-channel-node --mode read`)

Uses the same binary with `--mode read` (or `PRIVATE_CHANNEL_MODE=read`). Points to a PostgreSQL replica for read isolation.

### Gateway

**Source**: [`gateway/src/lib.rs`](../gateway/src/lib.rs)

| Flag | Env Var | Default | Description |
|------|---------|---------|-------------|
| `--port` | `GATEWAY_PORT` | `8898` | Listen port |
| `--internal-port` | `GATEWAY_INTERNAL_PORT` | — | Internal listen port; unset means no internal listener |
| `--write-url` | `GATEWAY_WRITE_URL` | — | Write node URL |
| `--read-url` | `GATEWAY_READ_URL` | — | Read node URL |
| `--cors-allowed-origin` | `GATEWAY_CORS_ALLOWED_ORIGIN` | `*` | CORS origin |

Routes `sendTransaction` to the write node; all other RPC methods go to the read node.

The internal port serves the operator's own services: no RBAC, no rate limiting,
and transaction errors are not collapsed. It must never be published to the host.
Compose gives it no `ports:` entry, which is the only thing keeping it internal.

**Internal services must never use `GATEWAY_URL`.** They carry no JWT, so once
RBAC is on the public port answers 401 to every gated read (`getBlock`,
`getAccountInfo`, `getSignaturesForAddress`) and indexing or minting stalls while
the stack still looks healthy. Use:

- `GATEWAY_INTERNAL_URL` if the service also sends transactions, since only the
  gateway routes writes to the write node.
- `GATEWAY_READ_URL` if it only reads. The read node has no auth layer at all.

### Streamer

**Source**: [`core/src/bin/streamer.rs`](../core/src/bin/streamer.rs)

| Flag | Env Var | Default | Description |
|------|---------|---------|-------------|
| `--port` | `PORT` (fallback: `STREAMER_PORT`) | `8902` | WebSocket listen port |
| `--accountsdb-connection-url` | `STREAMER_ACCOUNTSDB_CONNECTION_URL` | — | Solana Private Channels DB connection |
| `--poll-interval-ms` | `STREAMER_POLL_INTERVAL_MS` | `700` | DB polling interval (ms) |
| `--cors-allowed-origin` | `STREAMER_CORS_ALLOWED_ORIGIN` | `*` | CORS origin |

Exposes `/ws` for real-time transaction streaming and `/health` for health checks.

#### Accessing the streamer (internal-only)

The streamer feed is unauthenticated, so it is **not published to the host**. It listens only on the
`private-channel-network` Docker network at `ws://streamer:8902/ws` (`streamer` is the Compose service
DNS name; container `private-channel-streamer`).

To listen to the feed, attach a throwaway WebSocket client to the network:

```shell
docker run --rm --network private-channel_private-channel-network solsson/websocat ws://streamer:8902/ws
```

## Changing Configuration

Set the corresponding environment variable in your `.env.devnet` file (or equivalent) and restart the service:

```shell
# Example: increase sigverify workers to 16
PRIVATE_CHANNEL_SIGVERIFY_WORKERS=16

# Restart the write node to pick up changes
docker compose -f docker-compose.devnet.yml --env-file versions.env --env-file .env.devnet up -d write-node
```

## Restart & Recovery

### Write Node

On restart, the write node recovers state from PostgreSQL before accepting transactions:

**Source**: [`core/src/stages/dedup.rs`](../core/src/stages/dedup.rs), [`core/src/stages/settle.rs`](../core/src/stages/settle.rs)

1. **Dedup cache rebuild**: Reads the last N blocks (where N = `transaction_expiration_ms / blocktime_ms`) and reconstructs the signature dedup cache. This prevents duplicate transaction execution after restart. Failure here is fatal — the node will not start with an empty cache if blocks exist in the DB.

2. **Settlement state**: Queries `latest_slot` and `latest_blockhash` from the database to resume block production from the correct point.

3. **Redis cache alignment** (optional): If `--redis-cache-url` is configured, checks that the cache belongs to this ledger before the first write to it. The same flag wires the read path, so a writer cannot stop mirroring a cache that readers still trust. Each PostgreSQL database is stamped with a `deployment_id` at creation and the cache carries a copy; a cache naming a different deployment, or whose tip does not match PostgreSQL exactly, is emptied. The chain tip is then published and the cache stamped. Account, block and transaction state is not preloaded: a key missing from the cache is a miss that resolves against PostgreSQL. The two node roles differ on failure: a write node that cannot verify the cache drops it and runs PostgreSQL-only rather than writing a second ledger into it, while a read node refuses to start, because it cannot purge the cache itself and has no business serving what it found.

   The same stamp governs the cache while the node runs. Every cached read checks it, so clearing it takes the cache out of service on the next read. If a settled batch fails to reach the cache, the cached tip stops advancing; the next batch notices, clears the stamp and rebuilds the cache in the background, after which it is stamped again and serves normally. A cache purged on every startup usually means two deployments sharing one Redis instance. `private_channel_redis_cache_purged_total` is the signal.

The write node does not use a WAL — all state is deterministically recoverable from the PostgreSQL block history.

### Indexer

On restart, the indexer compares its last checkpoint slot against the current on-chain slot. If the gap exceeds the configured threshold, it triggers a parallel backfill before switching to real-time mode. See [Indexer Architecture](INDEXER.md) for details.

## Operational Tools

### Admin CLI

**Source**: [`core/src/bin/admin.rs`](../core/src/bin/admin.rs)

The `private-channel-admin` binary provides database maintenance commands:

```shell
# Truncate old blocks/transactions (requires recent backup)
private-channel-admin truncate --keep-slots 100000

# Dry run to preview what would be deleted
private-channel-admin truncate --keep-slots 100000 --dry-run
```

Truncation deletes in batches, and each batch commits the new retention floor
(`getFirstAvailableBlock`) in the same transaction as its deletions. A partial or
aborted run therefore never advertises history it has already removed.

### Makefile Targets

**Source**: [`Makefile`](../Makefile)

| Target | Description |
|--------|-------------|
| `make build` | Build all components |
| `make fmt` | Format and lint all code |
| `make unit-test` | Run unit tests |
| `make all-test` | Run all unit + integration tests |
| `make build-devnet` | Build programs for devnet |
| `make deploy-devnet` | Deploy programs to devnet |
| `make generate-clients` | Generate IDL and TypeScript/Rust clients |
| `make obs-up` / `make obs-down` | Start/stop observability stack (Prometheus, Grafana, cAdvisor) |
| `make obs-devnet-up` / `make obs-devnet-down` | Start/stop devnet observability stack |
| `make docker-up` / `make docker-down` | Start/stop the full local stack (wraps the `--env-file versions.env --env-file .env.local` chain) |
| `make docker-build` / `make docker-rebuild` | Build images / rebuild and restart the full local stack |
| `make docker-devnet-up` / `make docker-devnet-down` | Start/stop the full devnet stack (reads `.env.devnet`) |
| `make install-buildkit-cache` | One-time setup: install BuildKit GC config into `/etc/docker/daemon.json` (required before first `docker-build`) |

### Operational Scripts

**Source**: [`scripts/`](../scripts/)

| Script | Description |
|--------|-------------|
| `scripts/ensure-operator-keypair.sh` | Generate operator keypair if missing |
| `scripts/update-admin-env.sh` | Update `.env` with admin pubkey |
| `scripts/reconcile-escrow-balance.sh` | Reconcile on-chain vs DB escrow balances (supports alert webhooks) |
| `scripts/devnet/devnet-test.sh` | Full E2E test: instance creation through deposit/withdrawal/backfill validation |
