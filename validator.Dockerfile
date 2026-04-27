# Dockerfile for Solana Test Validator with Yellowstone gRPC Plugin
#
# Both SOLANA_VERSION and YELLOWSTONE_TAG come from versions.env.
# These two MUST move together — the Geyser plugin ABI is pinned to the
# Solana version the plugin was built against. Drift between CLI and plugin
# produces "validator starts then crashes on first subscribe".
# Build via: `docker compose --env-file versions.env --env-file .env build validator`
ARG SOLANA_VERSION
ARG YELLOWSTONE_TAG

FROM --platform=linux/amd64 rust:bookworm AS builder
ARG SOLANA_VERSION
ARG YELLOWSTONE_TAG

# Install Solana CLI at the pinned version.
RUN test -n "${SOLANA_VERSION}" || (echo "ERROR: SOLANA_VERSION build arg is required (use --env-file versions.env)" && exit 1) \
    && sh -c "$(curl -sSfL https://release.anza.xyz/v${SOLANA_VERSION}/install)"
ENV PATH="/root/.local/share/solana/install/active_release/bin:$PATH"

# Clone and build Yellowstone gRPC Geyser plugin
WORKDIR /build
RUN apt-get update && apt-get install -y \
    git \
    pkg-config \
    libssl-dev \
    protobuf-compiler \
    clang \
    cmake \
    && rm -rf /var/lib/apt/lists/*

# Checkout the tag that matches the installed Solana CLI.
# The Geyser plugin is dlopen()'d by solana-test-validator; ABI must match exactly.
RUN test -n "${YELLOWSTONE_TAG}" || (echo "ERROR: YELLOWSTONE_TAG build arg is required (use --env-file versions.env)" && exit 1) \
    && git clone https://github.com/rpcpool/yellowstone-grpc.git \
    && cd yellowstone-grpc \
    && git checkout "${YELLOWSTONE_TAG}" \
    && cargo build --release -p yellowstone-grpc-geyser

# Runtime stage
FROM --platform=linux/amd64 debian:bookworm-slim

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Copy Solana binaries and Yellowstone plugin from builder
COPY --from=builder /root/.local/share/solana/install/active_release /usr/local/solana
COPY --from=builder /build/yellowstone-grpc/target/release/libyellowstone_grpc_geyser.so /usr/local/lib/

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
