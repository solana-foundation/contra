# Railway Deployment

This guide covers deploying Solana Private Channels to [Railway](https://railway.com) as a multi-service project.

## Architecture

All services are built from a single Dockerfile that produces seven binaries: `private-channel-node`, `gateway`, `indexer`, `activity`, `streamer`, `admin`, and `auth`. Each Railway service runs the same Docker image with a different start command and environment variables. (`admin` is a CLI for one-off ops, not deployed as a long-running service.)

> **Note on `PRIVATE_CHANNEL_VERSION`:** The local compose stack consolidates all Rust services onto a single shared image tag (`private-channel-app:${PRIVATE_CHANNEL_VERSION:-latest}`) — see `docker-compose.yml`. **This sharing does not apply on Railway today.** Each Railway service has its own build pipeline and produces its own image, even though they all reference the same `Dockerfile`. `PRIVATE_CHANNEL_VERSION` is a local-only knob. To replicate the consolidated single-image pattern on Railway (build once, deploy to N services), see [`RAILWAY_SHARED_IMAGE.md`](RAILWAY_SHARED_IMAGE.md).

| Railway Service | Binary | Role |
|---|---|---|
| `write-node` | `private-channel-node` | Core write node (processes transactions) |
| `read-node` | `private-channel-node` | Core read node (serves queries) |
| `gateway` | `gateway` | Routes requests between write and read nodes |
| `streamer` | `streamer` | WebSocket server streaming real-time Solana Private Channels transactions to frontends |
| `indexer-solana` | `indexer` | Indexes Solana transactions via Yellowstone gRPC |
| `indexer-private_channel` | `indexer` | Indexes Solana Private Channels transactions via RPC polling |
| `operator-solana` | `indexer` | Processes escrow program operations |
| `operator-private_channel` | `indexer` | Processes withdrawal program operations |
| `auth` | `auth` | Optional RBAC service issuing JWTs to gateway clients (only when the `auth` profile is enabled) |
| `admin-ui` | Vite/React | Web UI for managing escrow instances, operators, mints, balances, and withdrawals |

Services **not** deployed to Railway:
- **PostgreSQL** -- use a Railway-managed Postgres instance instead
- **Solana Validator** -- connect to mainnet/devnet RPC
- **Activity Generator** -- load testing tool, not for production
- **cAdvisor** -- container metrics collector, not available on Railway (no Docker socket access)

## Prerequisites

- [Railway CLI](https://docs.railway.com/guides/cli) installed and authenticated (`railway login`)
- A Railway project linked to this repo (`railway link`)
- A Railway PostgreSQL instance in the project
- Generated clients and built programs:
  ```bash
  make install              # Install pnpm dependencies for both programs
  make generate-clients     # Generate IDL + Rust/JS clients from Codama annotations
  make -C private-channel-escrow-program build
  make -C private-channel-withdraw-program build
  ```
  The repo contains symlinks (`core/precompiles/private_channel_withdraw_program.so`, `test_utils/programs/*.so`) that point to `target/deploy/`. These must resolve or `railway up` will fail during upload.
- An admin keypair (e.g., `keypairs/admin.json`)

## Database Setup

The system uses two PostgreSQL databases. Connect to your Railway Postgres and create them:

```sql
CREATE DATABASE private_channel;
CREATE DATABASE indexer;
```

Schemas auto-initialize on first connection -- no migration files are needed.

## Creating Services

Use the Railway CLI to create all services:

```bash
railway add --service write-node
railway add --service read-node
railway add --service gateway
railway add --service indexer-solana
railway add --service indexer-private_channel
railway add --service operator-solana
railway add --service operator-private_channel
railway add --service streamer
railway add --service admin-ui
railway add --service blackbox-exporter
```

The `blackbox-exporter` service uses the Docker image `prom/blackbox-exporter:v0.25.0` directly (not the repo Dockerfile). In Railway, set **Settings > Build > Docker Image** to `prom/blackbox-exporter:v0.25.0`. No env vars, start command, or volumes needed -- it listens on port 9115. Keep this tag in lockstep with `BLACKBOX_VERSION` in [`versions.env`](../versions.env).

## Start Commands

Set the start command for each service in the Railway dashboard under **Settings > Deploy > Custom Start Command**:

| Service | Start Command |
|---|---|
| `write-node` | `/usr/local/bin/private-channel-node` |
| `read-node` | `/usr/local/bin/private-channel-node` |
| `gateway` | `/usr/local/bin/gateway` |
| `indexer-solana` | `/usr/local/bin/indexer --config /etc/private_channel/config/railway/indexer-solana.toml -v indexer` |
| `indexer-private_channel` | `/usr/local/bin/indexer --config /etc/private_channel/config/railway/indexer-private_channel.toml -v indexer` |
| `operator-solana` | `/usr/local/bin/indexer --config /etc/private_channel/config/railway/operator-solana.toml -v operator` |
| `operator-private_channel` | `/usr/local/bin/indexer --config /etc/private_channel/config/railway/operator-private_channel.toml -v operator` |
| `streamer` | `/usr/local/bin/streamer` |

Config files are baked into the Docker image at `/etc/private_channel/config/` during build.

The `admin-ui` service uses a separate Dockerfile (`admin-ui/Dockerfile`) and must be configured in the Railway dashboard:
- **Settings > Build > Dockerfile Path**: `admin-ui/Dockerfile`
- **Settings > Build > Docker Build Context**: `/` (repo root, needed so the generated TypeScript clients can be copied)
- **Settings > Build > Build Args** (mirror of `versions.env`, since Railway doesn't read it):
  - `PNPM_VERSION` — must match `versions.env` (currently `10.15.1`).
  - `NODE_VERSION` — must match `versions.env` (currently `24.7.0`); selects the `node:${NODE_VERSION}-slim` base image.

  Both have fallback defaults in the Dockerfile, but set them explicitly here so a `versions.env` bump in CI/Docker doesn't silently drift from Railway.
- No custom start command needed -- the Dockerfile handles it.

## Environment Variables

### write-node

| Variable | Value |
|---|---|
| `PRIVATE_CHANNEL_PORT` | `8900` |
| `PRIVATE_CHANNEL_ACCOUNTSDB_CONNECTION_URL` | `postgres://user:pass@host:port/private_channel` |
| `PRIVATE_CHANNEL_ENABLE_READ` | `false` |
| `PRIVATE_CHANNEL_MODE` | `write` |
| `PRIVATE_CHANNEL_SIGVERIFY_QUEUE_SIZE` | `10000000` |
| `PRIVATE_CHANNEL_SIGVERIFY_WORKERS` | `32` |
| `PRIVATE_CHANNEL_MAX_CONNECTIONS` | `1000000` |
| `PRIVATE_CHANNEL_MAX_TX_PER_BATCH` | `64` |
| `PRIVATE_CHANNEL_ADMIN_KEYS` | Admin pubkey(s), comma-separated |
| `PRIVATE_CHANNEL_LOG_LEVEL` | `info` |
| `PRIVATE_CHANNEL_JSON_LOGS` | `true` |
| `RUST_LOG` | `info` |

### read-node

| Variable | Value |
|---|---|
| `PRIVATE_CHANNEL_PORT` | `8901` |
| `PRIVATE_CHANNEL_ACCOUNTSDB_CONNECTION_URL` | `postgres://user:pass@host:port/private_channel` |
| `PRIVATE_CHANNEL_ENABLE_WRITE` | `false` |
| `PRIVATE_CHANNEL_ENABLE_READ` | `true` |
| `PRIVATE_CHANNEL_MODE` | `read` |
| `PRIVATE_CHANNEL_MAX_CONNECTIONS` | `100000` |
| `PRIVATE_CHANNEL_LOG_LEVEL` | `info` |
| `PRIVATE_CHANNEL_JSON_LOGS` | `true` |
| `RUST_LOG` | `info` |

> Both nodes share the same Postgres database. Without streaming replication (which a single Railway PG instance doesn't provide), the read node doesn't have replication-level isolation. This is fine for initial deployment.

### gateway

| Variable | Value |
|---|---|
| `GATEWAY_PORT` | `8899` |
| `GATEWAY_WRITE_URL` | `http://${{write-node.RAILWAY_PRIVATE_DOMAIN}}:8900` |
| `GATEWAY_READ_URL` | `http://${{read-node.RAILWAY_PRIVATE_DOMAIN}}:8901` |
| `RUST_LOG` | `info` |

### indexer-solana

| Variable | Value |
|---|---|
| `DATABASE_URL` | `postgres://user:pass@host:port/indexer` |
| `COMMON_RPC_URL` | Solana RPC endpoint |
| `COMMON_ESCROW_INSTANCE_ID` | Escrow instance pubkey |
| `INDEXER_YELLOWSTONE_ENDPOINT` | Yellowstone gRPC endpoint |
| `INDEXER_YELLOWSTONE_TOKEN` | Yellowstone auth token |
| `RUST_LOG` | `info,private_channel_indexer=debug` |

### indexer-private_channel

| Variable | Value |
|---|---|
| `DATABASE_URL` | `postgres://user:pass@host:port/indexer` |
| `COMMON_RPC_URL` | `http://${{gateway.RAILWAY_PRIVATE_DOMAIN}}:8899` |
| `RUST_LOG` | `info,private_channel_indexer=debug` |

### operator-solana

| Variable | Value |
|---|---|
| `DATABASE_URL` | `postgres://user:pass@host:port/indexer` |
| `COMMON_RPC_URL` | `http://${{gateway.RAILWAY_PRIVATE_DOMAIN}}:8899` (Solana Private Channels — where mint txs are sent) |
| `COMMON_SOURCE_RPC_URL` | Solana RPC endpoint (devnet — where escrow state is read) |
| `COMMON_ESCROW_INSTANCE_ID` | Escrow instance pubkey |
| `ADMIN_SIGNER` | `memory` (or `vault` / `turnkey` / `privy`) |
| `ADMIN_PRIVATE_KEY` | Admin private key (base58) |
| `RUST_LOG` | `info,private_channel_indexer=debug` |

### operator-private_channel

| Variable | Value |
|---|---|
| `DATABASE_URL` | `postgres://user:pass@host:port/indexer` |
| `COMMON_RPC_URL` | `http://${{gateway.RAILWAY_PRIVATE_DOMAIN}}:8899` |
| `ADMIN_SIGNER` | `memory` (or `vault` / `turnkey` / `privy`) |
| `ADMIN_PRIVATE_KEY` | Admin private key (base58) |
| `RUST_LOG` | `info,private_channel_indexer=debug` |

### streamer

The streamer polls the Solana Private Channels PostgreSQL database for new blocks/transactions and broadcasts them over WebSocket to connected frontends. It replaces the 4-second RPC polling the admin-ui previously used for the activity feed.

| Variable | Value |
|---|---|
| `STREAMER_PORT` | `8902` |
| `STREAMER_ACCOUNTSDB_CONNECTION_URL` | `postgres://user:pass@host:port/private_channel` (same DB as read-node) |
| `STREAMER_POLL_INTERVAL_MS` | `200` (how often to check for new blocks, in ms) |
| `STREAMER_CORS_ALLOWED_ORIGIN` | `*` |
| `STREAMER_LOG_LEVEL` | `info` |
| `STREAMER_JSON_LOGS` | `true` |
| `RUST_LOG` | `info` |

The streamer needs a **public domain** for the admin-ui (and any other frontend) to connect via WebSocket. Add one via **Settings > Networking > Generate Domain**. The WebSocket endpoint is `wss://<domain>/ws` and a health check is available at `https://<domain>/health`.

### admin-ui

The admin UI is a static React/Vite app. It connects to Solana RPC directly (via wallet) and to the Solana Private Channels gateway for payment channel operations.

| Variable | Kind | Value |
|---|---|---|
| `PRIVATE_CHANNEL_RPC_URL` | Runtime | Gateway public URL (e.g., `https://gateway-production-xxxx.up.railway.app`) |
| `PRIVATE_CHANNEL_WS_URL` | **Build arg** | Streamer WebSocket URL (e.g., `wss://streamer-production-xxxx.up.railway.app/ws`) |
| `PORT` | Runtime | `3000` |

`PRIVATE_CHANNEL_WS_URL` is baked into the JS bundle at Docker build time via a `Dockerfile ARG`. Railway passes service variables as build args automatically. The browser connects directly to the streamer over WebSocket, so this URL cannot be proxied through `server.mjs` like the RPC URLs are.

If you change the streamer URL later, redeploy the admin-ui so the new URL is embedded in the bundle. If `PRIVATE_CHANNEL_WS_URL` is not set, the fallback is `wss://streamer.example.com/ws` (placeholder — set `PRIVATE_CHANNEL_WS_URL` to your real streamer URL before deploying).

The admin-ui also needs a public domain (**Settings > Networking > Generate Domain**).

## Config Override System

The indexer/operator services use [Figment](https://github.com/SergioBenitez/Figment) for configuration. TOML config files provide structural defaults; environment variables override specific values:

| Env Prefix | Overrides |
|---|---|
| `COMMON_*` | `[common]` section (e.g., `COMMON_RPC_URL` overrides `common.rpc_url`) |
| `STORAGE_*` | `[storage]` section |
| `INDEXER_*` | `[indexer]` section (with nested handling for `YELLOWSTONE_*`, `RPC_POLLING_*`, `BACKFILL_*`) |
| `OPERATOR_*` | `[operator]` section |

`DATABASE_URL` and `INDEXER_YELLOWSTONE_TOKEN` are read directly from the environment, not through Figment.

## Deploying

Since the GitHub App integration may not see the repo, deploy each service with the CLI:

```bash
# Link to a service and push
railway service write-node && railway up
railway service read-node && railway up
railway service gateway && railway up
railway service indexer-solana && railway up
railway service indexer-private_channel && railway up
railway service operator-solana && railway up
railway service operator-private_channel && railway up
railway service streamer && railway up
railway service admin-ui && railway up
```

All services build from the same Dockerfile. After the first build, Railway caches Docker layers so subsequent deploys are faster.

## Networking

Services communicate over Railway's private network using `<service-name>.railway.internal`. Use Railway's `${{service.RAILWAY_PRIVATE_DOMAIN}}` variable references in the dashboard.

The **gateway**, **streamer**, and **admin-ui** need public domains. Add them via **Settings > Networking > Generate Domain** in the Railway dashboard. All other services stay internal-only.

## Post-Deploy: On-Chain Setup

After all services are deployed and running, the escrow system needs to be initialized on-chain before it can process deposits and withdrawals. These commands run **locally** against Solana devnet (not inside Railway). All scripts are in `scripts/devnet/`.

### Step 1: Create an Escrow Instance

This creates the on-chain escrow instance account that the system operates against.

```bash
cargo run --manifest-path scripts/devnet/Cargo.toml --bin create_instance -- \
  https://api.devnet.solana.com \
  ./keypairs/admin.json
```

This outputs an `escrow_instance_id` (a pubkey) and a transaction signature. **Save the instance ID** -- you'll need it for every subsequent step.

### Step 2: Add Operator

Authorize the admin keypair as an operator on the instance. This allows the operator services to process deposits and withdrawals.

```bash
cargo run --manifest-path scripts/devnet/Cargo.toml --bin add_operator -- \
  https://api.devnet.solana.com \
  ./keypairs/admin.json \
  <INSTANCE_ID> \
  <OPERATOR_PUBKEY>
```

`<OPERATOR_PUBKEY>` is the public key of the admin keypair:

```bash
solana-keygen pubkey ./keypairs/admin.json
```

### Step 3: Allow Mint

Whitelist the SPL token mint(s) the system will accept for deposits.

```bash
cargo run --manifest-path scripts/devnet/Cargo.toml --bin allow_mint -- \
  https://api.devnet.solana.com \
  ./keypairs/admin.json \
  <INSTANCE_ID> \
  <MINT_ADDRESS>
```

### Step 4: Update Railway Environment Variables

Now that you have the instance ID, set it on the services that need it. In the Railway dashboard or via CLI:

```bash
railway variable set COMMON_ESCROW_INSTANCE_ID=<INSTANCE_ID> --service indexer-solana
railway variable set COMMON_ESCROW_INSTANCE_ID=<INSTANCE_ID> --service operator-solana
```

Also set `PRIVATE_CHANNEL_ADMIN_KEYS` on the core nodes to the operator pubkey:

```bash
railway variable set PRIVATE_CHANNEL_ADMIN_KEYS=<OPERATOR_PUBKEY> --service write-node
railway variable set PRIVATE_CHANNEL_ADMIN_KEYS=<OPERATOR_PUBKEY> --service read-node
```

Setting variables triggers a redeploy automatically (unless `--skip-deploys` is used).

### Step 5: Generate a Gateway Domain

In the Railway dashboard, go to the **gateway** service > **Settings > Networking > Generate Domain**. This gives you a public URL like `gateway-production-xxxx.up.railway.app`.

This is your Solana Private Channels RPC endpoint. Use it in place of `http://localhost:8899` for withdrawals and any client interactions.

### Step 6: Verify

Test a deposit (runs on Solana devnet, depositing tokens into the escrow):

```bash
cargo run --manifest-path scripts/devnet/Cargo.toml --bin deposit -- \
  https://api.devnet.solana.com \
  ./keypairs/user.json \
  <INSTANCE_ID> \
  <MINT_ADDRESS> \
  <AMOUNT>
```

Test a withdrawal (runs against your Railway gateway, withdrawing from Solana Private Channels back to Solana):

```bash
cargo run --manifest-path scripts/devnet/Cargo.toml --bin withdraw -- \
  https://gateway-production-xxxx.up.railway.app \
  ./keypairs/user.json \
  <MINT_ADDRESS> \
  <AMOUNT>
```

Monitor processing via Railway logs:

```bash
railway logs --service operator-solana
railway logs --service operator-private_channel
```

### Setup Summary

| Step | What | Where |
|---|---|---|
| 1. Create instance | `create_instance` binary | Local, against Solana devnet |
| 2. Add operator | `add_operator` binary | Local, against Solana devnet |
| 3. Allow mint | `allow_mint` binary | Local, against Solana devnet |
| 4. Set instance ID | `COMMON_ESCROW_INSTANCE_ID` env var | Railway dashboard/CLI |
| 5. Set admin keys | `PRIVATE_CHANNEL_ADMIN_KEYS` env var | Railway dashboard/CLI |
| 6. Generate domain | Gateway public URL | Railway dashboard |
| 7. Test deposit | `deposit` binary | Local, against Solana devnet |
| 8. Test withdrawal | `withdraw` binary | Local, against Railway gateway |

## Extracting a Private Key

To convert a Solana keypair JSON file to a base58-encoded private key for use as `ADMIN_PRIVATE_KEY`:

```bash
node -e "
const kp = require('./keypairs/admin.json');
const ALPHABET = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
let bytes = Buffer.from(kp);
let n = BigInt('0x' + bytes.toString('hex'));
let result = '';
while (n > 0n) { const r = n % 58n; n = n / 58n; result = ALPHABET[Number(r)] + result; }
console.log(result);
"
```

## Observability

Prometheus and Grafana run as Railway services alongside the application services. The setup differs from local `docker-compose` in a few ways.

### Differences from Local

| Concern | Local (`docker-compose`) | Railway |
|---|---|---|
| Prometheus config | Single `prometheus.yml` with `${HOST_SUFFIX}` placeholders; `HOST_SUFFIX=` (empty) locally | `HOST_SUFFIX=.railway.internal` on Railway |
| Prometheus datasource URL | `http://prometheus:9090` (compose network) | `http://prometheus.railway.internal:9090` (set via `PROMETHEUS_URL` env var on grafana) |
| Postgres datasource URL | `postgres-indexer:5432` (compose network) | `postgres-nrto.railway.internal:5432` (set via `POSTGRES_INDEXER_HOST` env var on grafana) |
| cAdvisor container metrics | Available (Docker socket mounted) | Not available (no Docker socket on Railway) |
| Grafana dashboards | Mounted as read-only volume | Baked into Docker image via `Dockerfile.grafana` |

### Grafana Environment Variables

The grafana service needs these env vars to resolve datasource provisioning:

| Variable | Purpose | Example |
|---|---|---|
| `PROMETHEUS_URL` | Prometheus datasource URL | `http://prometheus.railway.internal:9090` |
| `POSTGRES_INDEXER_HOST` | Indexer Postgres hostname | `postgres-nrto.railway.internal` |
| `POSTGRES_INDEXER_DB` | Indexer database name | `indexer` |
| `POSTGRES_USER` | Postgres username | `postgres` |
| `POSTGRES_PASSWORD` | Postgres password | *(from Railway Postgres instance)* |
| `ALERT_WEBHOOK_URL` | Webhook URL for Grafana alerts | `https://hooks.slack.com/...` |

### Dashboards

| Dashboard | Panels | Datasources |
|---|---|---|
| Solana Private Channels Indexer | Infrastructure (CPU, memory, network, restarts), indexer metrics (slots, transactions, errors, lag), lag gauge | Prometheus |
| Solana Private Channels Operator | Infrastructure, operator metrics (fetched, backlog, sent, errors, DB updates), Solana RPC, withdrawal status breakdown, pending withdrawals over time | Prometheus, Postgres |
| Solana Private Channels RPC | Gateway request rates, latency, errors | Prometheus |

### Alerting

Grafana alert rules fire when any Solana Private Channels service becomes unreachable:

- **Scrape-based** (`service-down-scrape`) -- alerts when `up` metric drops to 0 for gateway, indexer-solana, indexer-private_channel, operator-solana, or operator-private_channel (services that expose Prometheus metrics).
- **Probe-based** (`service-down-probe`) -- alerts when blackbox-exporter's HTTP probe fails for write-node, read-node, or streamer `/health` endpoints.

Both rules use a 30s pending period. Worst-case detection is ~55s (15s scrape + 10s eval + 30s pending). Alerts auto-resolve when services recover. The existing webhook contact point (`ALERT_WEBHOOK_URL`) handles all alerts.

### Deploying Observability Services

```bash
railway up --service prometheus
railway up --service grafana
```

The `blackbox-exporter` service uses a stock Docker image -- no code deploy needed. Create it in the Railway dashboard with image `prom/blackbox-exporter:v0.25.0`.

Prometheus uses `Dockerfile.prometheus` which runs `envsubst` at startup to interpolate `${HOST_SUFFIX}` into the config. Set `HOST_SUFFIX=.railway.internal` on the Railway prometheus service. No custom start command is needed — the Dockerfile handles it.

## Files Added for Railway

| File | Purpose |
|---|---|
| `railway.toml` | Tells Railway to use the Dockerfile for builds |
| `indexer/config/railway/indexer-solana.toml` | Yellowstone indexer config (escrow program) |
| `indexer/config/railway/indexer-private_channel.toml` | RPC polling indexer config (withdraw program) |
| `indexer/config/railway/operator-solana.toml` | Operator config (escrow program) |
| `indexer/config/railway/operator-private_channel.toml` | Operator config (withdraw program) |
| `admin-ui/Dockerfile` | Separate Dockerfile for the React/Vite admin UI |
| `Dockerfile.grafana` | Grafana image with dashboards and provisioning baked in |
| `Dockerfile.prometheus` | Prometheus image with `envsubst` entrypoint for `HOST_SUFFIX` interpolation |
| `grafana/provisioning/datasources/prometheus.yml` | Grafana Prometheus datasource provisioning |
| `grafana/provisioning/datasources/postgres.yml` | Grafana Postgres datasource provisioning (indexer DB) |
| `grafana/dashboards/private-channel-indexer.json` | Indexer dashboard (infrastructure + indexer metrics + lag) |
| `grafana/dashboards/private-channel-operator.json` | Operator dashboard (operator metrics + withdrawals) |
| `grafana/dashboards/private-channel-rpc.json` | RPC/Gateway dashboard |
| `grafana/provisioning/alerting/alert-rules.yml` | Grafana alert rules (indexer lag + service-down) |

## Dockerfile Changes for Railway

- **Removed** `VOLUME` directive (Railway bans it; use Railway volumes instead)
- **Added** `COPY indexer/config /etc/private_channel/config` to include config files in the runtime image
- **Added** `RUN rm -f core/precompiles/private_channel_withdraw_program.so && cp target/deploy/private_channel_withdraw_program.so core/precompiles/private_channel_withdraw_program.so` to resolve the symlink during Docker build (the source `core/precompiles/private_channel_withdraw_program.so` is a symlink to `../../target/deploy/` which only exists after the program is built in the builder stage)
