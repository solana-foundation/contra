# Technical Requirements

This document outlines the hardware, software, and network requirements for running Contra infrastructure components.

---

## Hardware Requirements

*The following hardware estimates are theoretical projections based on per-component analysis (e.g., Ed25519 throughput per core). They have not been validated under sustained load. Use as sizing guidance, not guarantees.*

### Contra Write Node Requirements (per Contra instance)

The Contra Write Node is the primary bottleneck for transaction throughput. Requirements scale linearly with target TPS:

| Target TPS | CPU Cores | Memory | Network Bandwidth | 
|------------|-----------|--------|-------------------|
| **100k** | 2.3 | 8GB | 367 Mbps |  
| **200k** | 4.6 | 8GB | 733 Mbps |
| **300k** | 6.9 | 8GB | 1.10 Gbps |
| **400k** | 9.2 | 8GB | 1.47 Gbps |
| **500k** | 11.5 | 8GB | 1.83 Gbps |
| **600k** | 13.7 | 8GB | 2.20 Gbps |
| **700k** | 16.0 | 8GB | 2.56 Gbps |
| **800k** | 18.3 | 8GB | 2.93 Gbps |
| **900k** | 20.6 | 8GB | 3.30 Gbps |
| **1,000k** | 22.9 | 8GB | 3.66 Gbps |

**Notes:**
- CPU requirements are based on signature verification parallelization (~43,600 TPS per core)
- Memory remains constant at 8GB for most workloads
- Network bandwidth assumes sustained TPS with ~300-byte average transaction size
- Fractional cores should be rounded up to the next available vCPU count

### Storage Requirements by Token Accounts

Token account state is the primary driver of base storage requirements:

| Token Accounts | Disk Size Required |
|----------------|-------------------|
| **100k** | 16.5 MB |
| **200k** | 33.0 MB |
| **300k** | 49.5 MB |
| **400k** | 66.0 MB |
| **500k** | 82.5 MB |
| **600k** | 99.0 MB |
| **700k** | 115.5 MB |
| **800k** | 132.0 MB |
| **900k** | 148.5 MB |
| **1,000k** | 165.0 MB |

**Notes:**
- Assumes ~165 bytes per token account
- This is the baseline state storage requirement
- Additional storage is required for blocks/transactions (see retention schedule below)

### Storage Requirements by Block/Transaction Retention

At 100k sustained TPS, storage requirements grow based on retention period:

| Retention Duration | Disk Size Required |
|--------------------|-------------------|
| **1 day** | 4.51 TB |
| **2 days** | 9.02 TB |
| **3 days** | 13.53 TB |
| **4 days** | 18.04 TB |
| **5 days** | 22.55 TB |
| **6 days** | 27.06 TB |
| **7 days** | 31.57 TB |

**Notes:**
- Based on 100k TPS sustained load
- Storage scales linearly with TPS (200k TPS = 2x storage)
- Blocks and transactions can be truncated and backed up to cheaper archival storage
- **Recommended:** Retain 1-3 days online, archive older data to S3/GCS/Azure Blob

### Contra Read Node Requirements

Read nodes are horizontally scalable and run PostgreSQL read replicas:

| Component | CPU | RAM | Storage | Network |
|-----------|-----|-----|---------|---------|
| **Contra Read Node** | 4-8 cores | 8GB | Same as write node | Up to write node bandwidth |

**Notes:**
- CPU requirements are significantly lower than write nodes (no signature verification)
- Storage requirements match write nodes (full database replicas)
- Network bandwidth can approach write node levels if serving high query volumes with low eventual consistency latency
- Scale horizontally by adding more read replicas behind a load balancer

### Supporting Infrastructure

These components have minimal compute requirements relative to the write node. Specific benchmarks are pending, but general guidance:

| Component | Notes |
|-----------|-------|
| **PostgreSQL Primary** | Scales with write node TPS. NVMe storage recommended for WAL throughput. |
| **PostgreSQL Replica** | Matches primary storage. CPU scales with query load. |
| **Indexer (Mainnet)** | Lightweight — primarily I/O bound (RPC/gRPC reads + DB writes). 2-4 cores, 4GB RAM typical. |
| **Indexer (Contra)** | Same as Mainnet indexer. |
| **Operator (Mainnet)** | Lightweight — polls DB, builds and submits transactions. 2-4 cores, 4GB RAM typical. |
| **Operator (Contra)** | Same as Mainnet operator. |
| **Gateway** | Stateless RPC proxy. Scales horizontally. 2-4 cores, 2GB RAM typical. |
| **Streamer** | WebSocket transaction streamer. Scales with subscriber count. 2-4 cores, 2GB RAM typical. |


## Software Requirements

### Required Software

| Software | Minimum Version | Purpose |
|----------|----------------|---------|
| [**Rust**](https://rust-lang.org/tools/install/) | 1.91+ | Building Contra programs and core components (pinned via `rust-toolchain.toml`) |
| [**Solana CLI**](https://solana.com/docs/intro/installation) | 2.2.19+ (programs)<br/>2.3.9+ (Yellowstone) | Program deployment and interaction |
| [**Docker**](https://docs.docker.com/get-docker/) | 26.0+ | Containerized deployment |
| [**Docker Compose**](https://docs.docker.com/compose/install/) | 2.20+ | Multi-container orchestration |
| [**PostgreSQL**](https://www.postgresql.org/download/) | 16+ | Database (if not using Docker) |
| [**pnpm**](https://pnpm.io/installation) | 10.0+ | TypeScript client development |

### Operating System Support

Contra has been tested on the following operating systems:

- **Linux**: Ubuntu 22.04+, Debian 12+, RHEL 9+
- **macOS**: 15.0+

## Network Requirements

### Port Allocation

The following ports are used by Contra services by default:

| Service | Port | Protocol | Purpose |
|---------|------|----------|---------|
| **Contra Write Node** | 8900 | TCP | Transaction submission |
| **Contra Read Node** | 8901 | TCP | Account queries |
| **Gateway** | 8899 | TCP | Unified RPC endpoint |
| **PostgreSQL Primary** | 5432 | TCP | Database connections |
| **PostgreSQL Replica** | 5433 | TCP | Read-only database connections |
| **PostgreSQL Indexer** | 5434 | TCP | Indexer database connections |
| **Prometheus** | 9090 | TCP | Metrics collection |
| **Grafana** | 37429 | TCP | Metrics visualization |
| **cAdvisor** | 8080 | TCP | Container metrics |
| **Streamer** | 8902 | TCP | WebSocket transaction streaming |
| **Solana Validator** | 18899 | TCP | Local validator RPC (dev only) |
| **Yellowstone gRPC** | 10000 | TCP | Geyser streaming (dev only) |

### Firewall Configuration

**Inbound Rules** (Public-facing):
- Port 8899 (Gateway): Allow from application servers
- Ports 8900, 8901 (Contra Nodes): Allow from trusted IPs only

**Outbound Rules**:
- Port 443 (HTTPS): Allow to Solana Mainnet RPC endpoint(s)
- Port 10000 (gRPC): Allow to Yellowstone gRPC endpoint(s) (if using Yellowstone)

**Internal Rules** (Between services):
- Allow all traffic between Contra services on the same network
- PostgreSQL replication: Allow port 5432 from replica to primary



## Support

For questions about technical requirements or deployment assistance:

- **GitHub Issues**: https://github.com/solana-foundation/contra/issues
- **Stack Exchange**: Ask on https://solana.stackexchange.com/ (use the `contra` tag)
- **Documentation**: See [ARCHITECTURE.md](ARCHITECTURE.md) and [DEVNET_QUICKSTART.md](DEVNET_QUICKSTART.md)


