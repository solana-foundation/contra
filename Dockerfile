# Multi-stage Dockerfile for Contra blockchain

# Stage 1: Builder
FROM --platform=linux/amd64 rust:bookworm AS builder

# Install build dependencies and update to nightly
RUN apt-get update && apt-get install -y \
    clang \
    cmake \
    libhidapi-dev \
    libprotobuf-dev \
    libssl-dev \
    libudev-dev \
    pkg-config \
    protobuf-compiler \
    && rm -rf /var/lib/apt/lists/* \
    && rustup default nightly-2025-09-01 \
    && rustup component add rustfmt clippy

# Install Node.js and pnpm
RUN curl -fsSL https://deb.nodesource.com/setup_24.x | bash - \
    && apt-get install -y nodejs \
    && npm install -g pnpm@latest \
    && rm -rf /var/lib/apt/lists/*

# Install shank-cli
RUN cargo install shank-cli@0.4.5

# Install Solana CLI (stable version)
RUN sh -c "$(curl -sSfL https://release.anza.xyz/v2.2.19/install)" \
    && echo 'export PATH="$HOME/.local/share/solana/install/active_release/bin:$PATH"' >> ~/.bashrc
ENV PATH="/root/.local/share/solana/install/active_release/bin:${PATH}"

# Set working directory
WORKDIR /usr/src/contra

# Copy workspace cargo files first for better caching
COPY Cargo.toml Cargo.lock ./
COPY core/Cargo.toml ./core/
COPY gateway/Cargo.toml ./gateway/
COPY indexer/Cargo.toml ./indexer/

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

# Create dummy lib.rs files for workspace members we're not building
RUN mkdir -p contra-escrow-program/program/src contra-escrow-program/tests/integration-tests/src \
    contra-escrow-program/clients/rust/src contra-withdraw-program/program/src \
    contra-withdraw-program/tests/integration-tests/src \
    integration/src gateway/src indexer/src test_utils/src scripts/devnet/src \
    contra-escrow-program/clients/rust/src contra-withdraw-program/clients/rust/src \
    core/src
RUN touch contra-escrow-program/program/src/lib.rs contra-escrow-program/tests/integration-tests/src/lib.rs \
    contra-escrow-program/clients/rust/src/lib.rs contra-withdraw-program/program/src/lib.rs \
    contra-withdraw-program/tests/integration-tests/src/lib.rs \
    integration/src/lib.rs gateway/src/lib.rs indexer/src/lib.rs \
    test_utils/src/lib.rs scripts/devnet/src/lib.rs \
    contra-escrow-program/clients/rust/src/lib.rs contra-withdraw-program/clients/rust/src/lib.rs \
    core/src/lib.rs

# Build the project with the dummy files. We can cache this layer.
RUN cargo build --release

# First, do the real build for the programs
COPY Makefile ./Makefile
COPY contra-escrow-program ./contra-escrow-program
COPY contra-withdraw-program ./contra-withdraw-program
RUN make install
RUN make -C contra-escrow-program build
RUN make -C contra-withdraw-program build

# Next, do the real build for the other components
COPY core ./core
COPY gateway ./gateway
COPY indexer ./indexer

# Resolve the symlink: copy the built .so into core/precompiles/
# (the source symlink points to target/deploy/ which exists in the builder)
RUN cp -f target/deploy/contra_withdraw_program.so core/precompiles/contra_withdraw_program.so

RUN cargo build --release \
    -p contra-core \
    -p contra-gateway \
    -p contra-indexer

# Stage 2: Runtime
FROM --platform=linux/amd64 debian:bookworm-slim

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Create a non-root user to run the application
RUN useradd -m -u 1000 -s /bin/bash contra

# Copy the binaries from builder
COPY --from=builder /usr/src/contra/target/release/node /usr/local/bin/node
COPY --from=builder /usr/src/contra/target/release/activity /usr/local/bin/activity
COPY --from=builder /usr/src/contra/target/release/admin /usr/local/bin/admin
COPY --from=builder /usr/src/contra/target/release/gateway /usr/local/bin/gateway
COPY --from=builder /usr/src/contra/target/release/indexer /usr/local/bin/indexer
COPY --from=builder /usr/src/contra/target/release/streamer /usr/local/bin/streamer

# Copy indexer/operator config files
COPY indexer/config /etc/contra/config

# Create data directory for RocksDB
RUN mkdir -p /data && chown contra:contra /data

# Switch to non-root user
USER contra

# No default entrypoint - let docker-compose specify the command
# This ensures proper signal handling for graceful shutdown
