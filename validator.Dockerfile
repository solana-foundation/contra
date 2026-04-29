# syntax=docker/dockerfile:1.7
# Dockerfile for Solana Test Validator with Yellowstone gRPC Plugin
#
# Both SOLANA_VERSION and YELLOWSTONE_TAG come from versions.env.
# These two MUST move together — the Geyser plugin ABI is pinned to the
# Solana version the plugin was built against. Drift between CLI and plugin
# produces "validator starts then crashes on first subscribe".
# Build via: `docker compose --env-file versions.env --env-file .env build validator`
# Standalone build (outside compose): see README "Building a single Dockerfile standalone".
# Requires Docker >= 26.0 (BuildKit + the `--mount=type=cache` directives below).
ARG SOLANA_VERSION
ARG YELLOWSTONE_TAG

FROM --platform=linux/amd64 rust:bookworm AS builder
ARG SOLANA_VERSION
ARG YELLOWSTONE_TAG

# Disable the base image's apt auto-clean so the cache mount below persists downloaded .debs.
RUN rm -f /etc/apt/apt.conf.d/docker-clean \
    && echo 'Binary::apt::APT::Keep-Downloaded-Packages "true";' > /etc/apt/apt.conf.d/keep-cache

# Install Solana CLI at the pinned version.
RUN test -n "${SOLANA_VERSION}" || (echo "ERROR: SOLANA_VERSION build arg is required (use --env-file versions.env)" && exit 1) \
    && sh -c "$(curl -sSfL https://release.anza.xyz/v${SOLANA_VERSION}/install)"
ENV PATH="/root/.local/share/solana/install/active_release/bin:$PATH"

# Clone and build Yellowstone gRPC Geyser plugin
WORKDIR /build
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update && apt-get install -y \
    git \
    pkg-config \
    libssl-dev \
    protobuf-compiler \
    clang \
    cmake

# Clone in a separate RUN step. If we put the clone inside the cargo-build RUN below,
# the `--mount=type=cache,target=/build/yellowstone-grpc/target` mount causes BuildKit
# to pre-create /build/yellowstone-grpc/ — and `git clone` then refuses because the
# destination is non-empty.
RUN test -n "${YELLOWSTONE_TAG}" || (echo "ERROR: YELLOWSTONE_TAG build arg is required (use --env-file versions.env)" && exit 1) \
    && git clone https://github.com/rpcpool/yellowstone-grpc.git \
    && cd yellowstone-grpc \
    && git checkout "${YELLOWSTONE_TAG}"

# The Geyser plugin is dlopen()'d by solana-test-validator; ABI must match the CLI exactly.
# target/ is a cache mount, so the produced .so is copied out to /out/ before the next
# stage references it (cache mounts are invisible to `COPY --from=builder`).
RUN --mount=type=cache,target=/build/yellowstone-grpc/target,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    cd yellowstone-grpc \
    && cargo build --release -p yellowstone-grpc-geyser \
    && mkdir -p /out \
    && cp target/release/libyellowstone_grpc_geyser.so /out/

# Runtime stage
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

# Copy Solana binaries and Yellowstone plugin from builder
COPY --from=builder /root/.local/share/solana/install/active_release /usr/local/solana
COPY --from=builder /out/libyellowstone_grpc_geyser.so /usr/local/lib/

ENV PATH="/usr/local/solana/bin:$PATH"

# Create directory for validator data and config
RUN mkdir -p /validator-data /config

WORKDIR /validator-data

# Expose ports
# 8899: RPC
# 8900: RPC Pub/Sub
# 10000: Yellowstone gRPC
# 8999: Prometheus metrics
EXPOSE 8899 8900 10000 8999

# Default command (override in docker-compose)
CMD ["solana-test-validator"]
