FROM node:24-slim AS builder

RUN npm install -g pnpm@latest

WORKDIR /workspace

# Copy program dirs (IDL + codama scripts + package configs)
COPY contra-escrow-program ./contra-escrow-program
COPY contra-withdraw-program ./contra-withdraw-program

# Generate TypeScript clients from IDL
RUN cd contra-escrow-program && pnpm install && pnpm run generate-clients
RUN cd contra-withdraw-program && pnpm install && pnpm run generate-clients

# Copy and build admin-ui
COPY admin-ui ./admin-ui

WORKDIR /workspace/admin-ui
RUN pnpm install --frozen-lockfile

ARG CONTRA_RPC_URL
ARG CONTRA_WS_URL

RUN npx vite build

# Runtime
FROM node:24-slim

WORKDIR /app
COPY --from=builder /workspace/admin-ui/dist ./dist
COPY admin-ui/server.mjs .

CMD ["node", "server.mjs"]
