# syntax=docker/dockerfile:1.7
# Requires Docker >= 26.0 (BuildKit + the `--mount=type=cache` directives below).
#
# PNPM_VERSION: versions.env is the source of truth (consumed by docker compose). Railway
# builds don't read versions.env, so a default is set below as a fallback — Railway should
# still set this explicitly via Build Args so a versions.env bump doesn't silently drift.
# Bump this default in lockstep with versions.env when raising the pinned pnpm version.
FROM node:24-slim AS builder
ARG PNPM_VERSION=10.15.1

# Pin pnpm's content-addressed store to a known path so the cache mount below targets it.
ENV PNPM_HOME=/pnpm
ENV PATH="$PNPM_HOME:$PATH"
RUN npm install -g pnpm@${PNPM_VERSION} \
    && pnpm config set store-dir /pnpm-store --global

WORKDIR /workspace

# Copy program dirs (IDL + codama scripts + package configs)
COPY contra-escrow-program ./contra-escrow-program
COPY contra-withdraw-program ./contra-withdraw-program

# Generate TypeScript clients from IDL. The pnpm store is a content-addressed cache;
# node_modules under each project still get materialized as a normal layer.
RUN --mount=type=cache,target=/pnpm-store,sharing=locked \
    cd contra-escrow-program && pnpm install && pnpm run generate-clients
RUN --mount=type=cache,target=/pnpm-store,sharing=locked \
    cd contra-withdraw-program && pnpm install && pnpm run generate-clients

# Copy and build admin-ui
COPY admin-ui ./admin-ui

WORKDIR /workspace/admin-ui
RUN --mount=type=cache,target=/pnpm-store,sharing=locked \
    pnpm install --frozen-lockfile

ARG CONTRA_RPC_URL
ARG CONTRA_WS_URL

RUN npx vite build

# Runtime
FROM node:24-slim

WORKDIR /app
COPY --from=builder /workspace/admin-ui/dist ./dist
COPY admin-ui/server.mjs .

CMD ["node", "server.mjs"]
