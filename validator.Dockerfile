# Dockerfile for Solana Test Validator with Yellowstone gRPC Plugin
FROM --platform=linux/amd64 rust:bookworm AS builder

# Install Solana CLI
RUN sh -c "$(curl -sSfL https://release.anza.xyz/v2.3.9/install)"
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

RUN git clone https://github.com/rpcpool/yellowstone-grpc.git && \
    cd yellowstone-grpc && \
    git checkout v9.1.0+solana.2.3.11 && \
    cargo build --release -p yellowstone-grpc-geyser

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
