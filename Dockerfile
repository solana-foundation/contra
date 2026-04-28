# syntax=docker/dockerfile:1.7
# Multi-stage Dockerfile for Contra blockchain
#
# SOLANA_VERSION is the source of truth in versions.env.
# Build via: `docker compose --env-file versions.env --env-file .env build <service>`
# Requires Docker >= 26.0 (BuildKit + the `--mount=type=cache` directives below).

ARG SOLANA_VERSION
ARG PNPM_VERSION

# Stage 1: Builder
FROM --platform=linux/amd64 rust:bookworm AS builder
ARG SOLANA_VERSION
ARG PNPM_VERSION

# Disable the base image's apt auto-clean so the cache mount below persists downloaded .debs.
RUN rm -f /etc/apt/apt.conf.d/docker-clean \
    && echo 'Binary::apt::APT::Keep-Downloaded-Packages "true";' > /etc/apt/apt.conf.d/keep-cache

# Install build dependencies and update to nightly
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update && apt-get install -y \
    clang \
    cmake \
    libhidapi-dev \
    libprotobuf-dev \
    libssl-dev \
    libudev-dev \
    pkg-config \
    protobuf-compiler \
    && rustup default nightly-2025-09-01 \
    && rustup component add rustfmt clippy

# Install Node.js and pnpm
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    curl -fsSL https://deb.nodesource.com/setup_24.x | bash - \
    && apt-get install -y nodejs \
    && test -n "${PNPM_VERSION}" || (echo "ERROR: PNPM_VERSION build arg is required (use --env-file versions.env)" && exit 1) \
    && npm install -g pnpm@${PNPM_VERSION}

# Install Solana CLI — version driven by versions.env (SOLANA_VERSION).
# Drifting this version from the validator image or from Cargo.toml's solana-* crates
# reproduces the version-matrix bug that motivated consolidating into versions.env.
RUN test -n "${SOLANA_VERSION}" || (echo "ERROR: SOLANA_VERSION build arg is required (use --env-file versions.env)" && exit 1) \
    && sh -c "$(curl -sSfL https://release.anza.xyz/v${SOLANA_VERSION}/install)" \
    && echo 'export PATH="$HOME/.local/share/solana/install/active_release/bin:$PATH"' >> ~/.bashrc
ENV PATH="/root/.local/share/solana/install/active_release/bin:${PATH}"

# Convention used throughout this builder stage: build artifacts are copied into /out/
# before the next stage references them.
#
# Why: the cargo build steps below mount /usr/src/contra/target as a BuildKit cache
# (`--mount=type=cache`), which is *not* visible to later stages' `COPY --from=builder`
# and is also not visible to subsequent RUN steps that don't re-mount it. /out/ is a
# normal image layer, so artifacts placed there persist across RUN steps and are
# reachable from the runtime stage.

# Set working directory
WORKDIR /usr/src/contra

# Copy workspace cargo files first for better caching
COPY Cargo.toml Cargo.lock ./
COPY core/Cargo.toml ./core/
COPY gateway/Cargo.toml ./gateway/
COPY indexer/Cargo.toml ./indexer/
COPY auth/Cargo.toml ./auth/

# Copy Cargo.toml files for other workspace members (to satisfy workspace references)
COPY contra-escrow-program/program/Cargo.toml ./contra-escrow-program/program/
COPY contra-escrow-program/tests/integration-tests/Cargo.toml ./contra-escrow-program/tests/integration-tests/
COPY contra-escrow-program/clients/rust/Cargo.toml ./contra-escrow-program/clients/rust/
COPY contra-withdraw-program/program/Cargo.toml ./contra-withdraw-program/program/
COPY contra-withdraw-program/tests/integration-tests/Cargo.toml ./contra-withdraw-program/tests/integration-tests/
COPY contra-withdraw-program/clients/rust/Cargo.toml ./contra-withdraw-program/clients/rust/
COPY integration/Cargo.toml ./integration/
COPY test_utils/Cargo.toml ./test_utils/
COPY scripts/devnet/Cargo.toml ./scripts/devnet/
COPY metrics/Cargo.toml ./metrics/
COPY bench-tps/Cargo.toml ./bench-tps/

# Create dummy lib.rs files for workspace members we're not building
RUN mkdir -p contra-escrow-program/program/src contra-escrow-program/tests/integration-tests/src \
    contra-escrow-program/clients/rust/src contra-withdraw-program/program/src \
    contra-withdraw-program/tests/integration-tests/src \
    integration/src gateway/src indexer/src test_utils/src scripts/devnet/src \
    contra-escrow-program/clients/rust/src contra-withdraw-program/clients/rust/src \
    core/src metrics/src auth/src bench-tps/src
RUN touch contra-escrow-program/program/src/lib.rs contra-escrow-program/tests/integration-tests/src/lib.rs \
    contra-escrow-program/clients/rust/src/lib.rs contra-withdraw-program/program/src/lib.rs \
    contra-withdraw-program/tests/integration-tests/src/lib.rs \
    integration/src/lib.rs gateway/src/lib.rs indexer/src/lib.rs \
    test_utils/src/lib.rs scripts/devnet/src/lib.rs \
    contra-escrow-program/clients/rust/src/lib.rs contra-withdraw-program/clients/rust/src/lib.rs \
    core/src/lib.rs metrics/src/lib.rs auth/src/lib.rs && \
    printf 'fn main() {}\n' > bench-tps/src/main.rs && \
    printf 'fn main() {}\n' > auth/src/main.rs

# Build the project with the dummy files. We can cache this layer.
# Cache mounts: target/ holds compiled artifacts; cargo registry/git hold downloaded crate sources.
# All three are reused across rebuilds, turning a cold ~30 min build into <2 min when only
# source changes.
RUN --mount=type=cache,target=/usr/src/contra/target,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    cargo build --release

# First, do the real build for the programs
COPY Makefile ./Makefile
COPY contra-escrow-program ./contra-escrow-program
COPY contra-withdraw-program ./contra-withdraw-program
RUN --mount=type=cache,target=/usr/src/contra/target,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    make install \
    && make -C contra-escrow-program build \
    && make -C contra-withdraw-program build \
    && mkdir -p /out/deploy \
    && cp target/deploy/contra_escrow_program.so /out/deploy/ \
    && cp target/deploy/contra_withdraw_program.so /out/deploy/

# Next, do the real build for the other components
COPY core ./core
COPY gateway ./gateway
COPY indexer ./indexer
COPY metrics ./metrics
COPY auth ./auth

# core/precompiles/contra_withdraw_program.so is a symlink into target/deploy/ (used by
# include_bytes! in core). The cache-mounted target/ isn't reliably available to the next
# build, so swap the symlink for the real .so. rm first — otherwise cp follows the symlink
# and writes to the wrong place.
RUN rm -f core/precompiles/contra_withdraw_program.so \
    && cp /out/deploy/contra_withdraw_program.so core/precompiles/contra_withdraw_program.so

# Final build — binaries are copied to /out/ per the convention noted above.
RUN --mount=type=cache,target=/usr/src/contra/target,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    cargo build --release \
        -p contra-core \
        -p contra-gateway \
        -p contra-indexer \
        -p auth \
    && mkdir -p /out \
    && cp target/release/node /out/node \
    && cp target/release/activity /out/activity \
    && cp target/release/admin /out/admin \
    && cp target/release/gateway /out/gateway \
    && cp target/release/indexer /out/indexer \
    && cp target/release/streamer /out/streamer \
    && cp target/release/auth /out/auth

# Stage 2: Runtime
FROM --platform=linux/amd64 debian:bookworm-slim

# Disable the base image's apt auto-clean so the cache mount below persists downloaded .debs.
RUN rm -f /etc/apt/apt.conf.d/docker-clean \
    && echo 'Binary::apt::APT::Keep-Downloaded-Packages "true";' > /etc/apt/apt.conf.d/keep-cache

# Install runtime dependencies
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update && apt-get install -y \
    ca-certificates \
    libssl3

# Create a non-root user to run the application
RUN useradd -m -u 1000 -s /bin/bash contra

# Copy the binaries from builder. Source paths are /out/ (a normal layer in the builder
# stage), not target/release/ (a cache mount which is not visible across stages).
COPY --from=builder /out/node /usr/local/bin/contra-node
COPY --from=builder /out/activity /usr/local/bin/activity
COPY --from=builder /out/admin /usr/local/bin/admin
COPY --from=builder /out/gateway /usr/local/bin/gateway
COPY --from=builder /out/indexer /usr/local/bin/indexer
COPY --from=builder /out/streamer /usr/local/bin/streamer
COPY --from=builder /out/auth /usr/local/bin/auth

# Copy indexer/operator config files
COPY indexer/config /etc/contra/config

# Create data directory for RocksDB
RUN mkdir -p /data && chown contra:contra /data

# Switch to non-root user
USER contra

# No default entrypoint - let docker-compose specify the command
# This ensures proper signal handling for graceful shutdown
