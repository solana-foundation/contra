#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BENCH_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
REPO_ROOT="$(cd "${BENCH_DIR}/.." && pwd)"
BENCH_BIN="${BENCH_DIR}/target/release/contra-bench-tps"
BENCH_ENV="${BENCH_DIR}/.env"

# ---- Flags ---------------------------------------------------------------
# Consume --rebuild before forwarding remaining args to the bench binary.

REBUILD=0
CLEAN=0
BENCH_ARGS=()
for arg in "$@"; do
    case "${arg}" in
        --rebuild) REBUILD=1 ;;
        --clean)   CLEAN=1 ;;
        *)         BENCH_ARGS+=("${arg}") ;;
    esac
done

# ---- Sanity checks -------------------------------------------------------

if [ ! -f "${BENCH_BIN}" ]; then
    echo "ERROR: binary not found at ${BENCH_BIN}" >&2
    echo "       Run: cargo build --release --manifest-path bench-tps/Cargo.toml" >&2
    exit 1
fi

if [ ! -f "${BENCH_ENV}" ]; then
    echo "ERROR: ${BENCH_ENV} not found" >&2
    echo "       Run: cp ${BENCH_DIR}/.env.sample ${BENCH_ENV} and fill in the values" >&2
    exit 1
fi

# ---- Load bench env -------------------------------------------------------

# shellcheck disable=SC1091
set -a; source "${BENCH_ENV}"; set +a

WRITE_PORT="${CONTRA_WRITE_PORT:-8899}"
GATEWAY_PORT="${GATEWAY_PORT:-8898}"

# ---- CPU affinity split (75% services / 25% bench) -----------------------

TOTAL_CORES=$(nproc)

if [ "${TOTAL_CORES}" -lt 2 ]; then
    echo "WARNING: only ${TOTAL_CORES} core(s) detected — skipping CPU pinning" >&2
    SERVICE_CPUSET=""
    BENCH_CPUSET=""
else
    SERVICE_COUNT=$(( TOTAL_CORES * 3 / 4 ))
    [ "${SERVICE_COUNT}" -lt 1 ] && SERVICE_COUNT=1
    [ "${SERVICE_COUNT}" -ge "${TOTAL_CORES}" ] && SERVICE_COUNT=$(( TOTAL_CORES - 1 ))

    BENCH_START="${SERVICE_COUNT}"
    BENCH_END=$(( TOTAL_CORES - 1 ))

    SERVICE_CPUSET="0-$(( SERVICE_COUNT - 1 ))"
    BENCH_CPUSET="${BENCH_START}-${BENCH_END}"

    echo "CPUs: total=${TOTAL_CORES}  services=[${SERVICE_CPUSET}]  bench=[${BENCH_CPUSET}]"
fi

# ---- Docker compose command array ----------------------------------------
# Using an array avoids word-splitting issues if paths ever contain spaces.

COMPOSE=(docker compose -f "${REPO_ROOT}/docker-compose.yml" --env-file "${BENCH_ENV}")

# ---- Admin keypair -------------------------------------------------------
# Generate a persistent admin keypair for the bench.  The public key is written
# into CONTRA_ADMIN_KEYS so the write-node accepts admin transactions, and the
# private key bytes are written to ADMIN_PRIVATE_KEY for the operator/activity
# services.  Both values are patched directly into the .env file so docker
# compose picks them up at startup.

if ! command -v solana-keygen > /dev/null 2>&1; then
    echo "ERROR: solana-keygen not found in PATH" >&2
    echo "       Install the Solana CLI or add it to PATH before running run.sh" >&2
    exit 1
fi

ADMIN_KEYPAIR_FILE="${BENCH_DIR}/admin-keypair.json"

# Regenerate on --rebuild; otherwise reuse an existing keypair across runs so
# the write-node config stays valid without restarting.
if [ "${REBUILD}" -eq 1 ] || [ ! -f "${ADMIN_KEYPAIR_FILE}" ]; then
    solana-keygen new --no-bip39-passphrase --silent --force --outfile "${ADMIN_KEYPAIR_FILE}"
    echo "Generated admin keypair: ${ADMIN_KEYPAIR_FILE}"
else
    echo "Reusing existing admin keypair: ${ADMIN_KEYPAIR_FILE}"
fi

ADMIN_PUBKEY=$(solana-keygen pubkey "${ADMIN_KEYPAIR_FILE}")
# Flatten the JSON bytes array to a single line for use as an env var.
ADMIN_PRIVKEY_JSON=$(tr -d '[:space:]' < "${ADMIN_KEYPAIR_FILE}")

echo "Admin pubkey: ${ADMIN_PUBKEY}"

# Patch the .env file in-place so docker compose sees the values.
patch_env() {
    local key="$1"
    local value="$2"
    if grep -q "^${key}=" "${BENCH_ENV}"; then
        sed -i "s|^${key}=.*|${key}=${value}|" "${BENCH_ENV}"
    else
        echo "${key}=${value}" >> "${BENCH_ENV}"
    fi
}

patch_env "CONTRA_ADMIN_KEYS" "${ADMIN_PUBKEY}"
patch_env "ADMIN_PRIVATE_KEY" "${ADMIN_PRIVKEY_JSON}"

echo "Patched CONTRA_ADMIN_KEYS and ADMIN_PRIVATE_KEY in ${BENCH_ENV}"

# Re-source the env so the rest of the script sees the updated values.
# shellcheck disable=SC1091
set -a; source "${BENCH_ENV}"; set +a

# ---- Solana program build ------------------------------------------------
# The validator mounts target/deploy/*.so at startup.
# Build them if missing or if --rebuild was requested.

ESCROW_SO="${REPO_ROOT}/target/deploy/contra_escrow_program.so"
WITHDRAW_SO="${REPO_ROOT}/target/deploy/contra_withdraw_program.so"

programs_exist() {
    [ -f "${ESCROW_SO}" ] && [ -f "${WITHDRAW_SO}" ]
}

if [ "${REBUILD}" -eq 1 ]; then
    echo "Building Solana programs (--rebuild flag set)..."
    make -C "${REPO_ROOT}/contra-escrow-program" build
    make -C "${REPO_ROOT}/contra-withdraw-program" build
elif ! programs_exist; then
    echo "Solana .so files not found — building programs (this takes a few minutes)..."
    make -C "${REPO_ROOT}/contra-escrow-program" build
    make -C "${REPO_ROOT}/contra-withdraw-program" build
else
    echo "Solana .so files found — skipping program build"
fi

# ---- Docker image build --------------------------------------------------
# Built service images are named <project>-<service> by compose.
# The project name defaults to the repo directory name: "contra".

BUILT_IMAGES=(contra-write-node contra-read-node contra-gateway contra-streamer contra-activity contra-validator contra-indexer-solana contra-indexer-contra contra-operator-solana contra-operator-contra contra-prometheus)
BUILT_SERVICES=(write-node read-node gateway streamer activity validator indexer-solana indexer-contra operator-solana operator-contra prometheus)

images_exist() {
    docker image inspect "${BUILT_IMAGES[@]}" > /dev/null 2>&1
}

if [ "${REBUILD}" -eq 1 ]; then
    echo "Rebuilding images (--rebuild flag set)..."
    "${COMPOSE[@]}" build "${BUILT_SERVICES[@]}"
elif ! images_exist; then
    echo "Images not found — building for the first time (this takes a few minutes)..."
    "${COMPOSE[@]}" build "${BUILT_SERVICES[@]}"
else
    echo "Images found — skipping build"
fi

# ---- Cleanup on exit ------------------------------------------------------

cleanup() {
    echo ""
    echo "Stopping all services..."
    "${COMPOSE[@]}" stop 2>/dev/null || true
    echo "Done."
}
trap cleanup EXIT

# ---- Start all services ---------------------------------------------------
# --no-build: skip context re-evaluation entirely — images are already present.

# ---- Clean data volumes (--clean flag) -----------------------------------
# Wipes all postgres/validator data volumes so the next run starts from a
# clean state.  Use this when a previous run was interrupted mid-write and
# left corrupt data (e.g. "unexpected end of file" on startup).

if [ "${CLEAN}" -eq 1 ]; then
    echo "Removing data volumes (--clean flag set)..."
    "${COMPOSE[@]}" down -v 2>/dev/null || true
    echo "Data volumes removed."
fi

# ---- Fix WAL archive volume permissions ----------------------------------
# The postgres-indexer-wal-archive volume is created with root ownership by
# default; the postgres user (uid 70 on Alpine) cannot write to it without
# this fix.

for vol in postgres-indexer-wal-archive postgres-primary-wal-archive; do
    docker run --rm -v "${vol}:/vol" postgres:16-alpine \
        chown postgres:postgres /vol 2>/dev/null \
        && echo "Fixed permissions on volume ${vol}" \
        || echo "WARNING: could not fix permissions on ${vol} (may not exist yet)"
done

# ---- Start all services ---------------------------------------------------

echo "Starting all services..."
"${COMPOSE[@]}" up -d --no-build

# Pin all contra containers to service cores
if [ -n "${SERVICE_CPUSET}" ]; then
    echo "Pinning containers to cores [${SERVICE_CPUSET}]..."
    while IFS= read -r container; do
        docker update --cpuset-cpus="${SERVICE_CPUSET}" "${container}" 2>/dev/null \
            && echo "  pinned ${container}" \
            || echo "  WARNING: could not pin ${container}"
    done < <(docker ps --filter "name=contra-" --format "{{.Names}}")
fi

# ---- Wait helpers ---------------------------------------------------------

wait_healthy() {
    local container="$1"
    local max_wait=120
    local elapsed=0
    printf "Waiting for %s to be healthy..." "${container}"
    while [ "${elapsed}" -lt "${max_wait}" ]; do
        status=$(docker inspect --format='{{.State.Health.Status}}' "${container}" 2>/dev/null || echo "missing")
        if [ "${status}" = "healthy" ]; then
            echo " ok"
            return 0
        fi
        sleep 2
        elapsed=$(( elapsed + 2 ))
        printf "."
    done
    echo ""
    echo "ERROR: ${container} did not become healthy within ${max_wait}s" >&2
    return 1
}

# Probes a Solana JSON-RPC endpoint directly with a POST request.
# Succeeds as soon as the node responds (any HTTP 200).
wait_rpc() {
    local label="$1"
    local url="$2"
    local container="$3"
    local max_wait=180
    local elapsed=0
    printf "Waiting for %s at %s..." "${label}" "${url}"
    while [ "${elapsed}" -lt "${max_wait}" ]; do
        if curl -sf -X POST -H "Content-Type: application/json" \
            -d '{"jsonrpc":"2.0","id":1,"method":"getLatestBlockhash"}' \
            "${url}" > /dev/null 2>&1; then
            echo " ok"
            return 0
        fi
        # Fail fast if the container has crashed
        local state
        state=$(docker inspect --format='{{.State.Status}}' "${container}" 2>/dev/null || echo "missing")
        if [ "${state}" = "exited" ] || [ "${state}" = "dead" ] || [ "${state}" = "missing" ]; then
            echo ""
            echo "ERROR: ${container} has stopped (state=${state})" >&2
            echo "       Run: docker logs ${container}" >&2
            return 1
        fi
        sleep 2
        elapsed=$(( elapsed + 2 ))
        printf "."
    done
    echo ""
    echo "ERROR: ${label} did not respond within ${max_wait}s" >&2
    echo "       Run: docker logs ${container}" >&2
    return 1
}

# Waits for a container to reach the running state (no Docker healthcheck).
# Fails fast if the container exits/crashes.
wait_running() {
    local container="$1"
    local max_wait="${2:-60}"
    local elapsed=0
    printf "Waiting for %s to be running..." "${container}"
    while [ "${elapsed}" -lt "${max_wait}" ]; do
        state=$(docker inspect --format='{{.State.Status}}' "${container}" 2>/dev/null || echo "missing")
        if [ "${state}" = "running" ]; then
            echo " ok"
            return 0
        fi
        if [ "${state}" = "exited" ] || [ "${state}" = "dead" ]; then
            echo ""
            echo "ERROR: ${container} exited unexpectedly" >&2
            echo "       Run: docker logs ${container}" >&2
            return 1
        fi
        sleep 2
        elapsed=$(( elapsed + 2 ))
        printf "."
    done
    echo ""
    echo "ERROR: ${container} did not reach running state within ${max_wait}s" >&2
    return 1
}

# Used for the gateway: it exposes GET /health returning {"status":"ok"}.
# This checks the gateway process itself — not a proxied backend call.
wait_http_health() {
    local label="$1"
    local url="$2"
    local max_wait=60
    local elapsed=0
    printf "Waiting for %s at %s..." "${label}" "${url}"
    while [ "${elapsed}" -lt "${max_wait}" ]; do
        if curl -sf "${url}" > /dev/null 2>&1; then
            echo " ok"
            return 0
        fi
        sleep 2
        elapsed=$(( elapsed + 2 ))
        printf "."
    done
    echo ""
    echo "ERROR: ${label} did not respond within ${max_wait}s" >&2
    return 1
}

READ_PORT="${CONTRA_READ_PORT:-8900}"

# ---- Wait for stable state ------------------------------------------------

# Databases (Docker-managed pg_isready healthchecks)
wait_healthy "contra-postgres-primary"
wait_healthy "contra-postgres-replica"
wait_healthy "contra-postgres-indexer"

# Validator (healthcheck: solana cluster-version)
# Runs without --geyser-plugin-config via the bench overlay — avoids the
# Yellowstone gRPC plugin SIGSEGV on Solana 2.3.9.
wait_healthy "contra-validator"

# Pipeline nodes — probed directly so we know the full DB→migration→RPC path is live
wait_rpc "write-node" "http://localhost:${WRITE_PORT}" "contra-write-node"
wait_rpc "read-node"  "http://localhost:${READ_PORT}"  "contra-read-node"

# Gateway (own /health endpoint, not proxied)
wait_http_health "gateway" "http://localhost:${GATEWAY_PORT}/health"

# Remaining services — just confirm they are running (not exited)
wait_running "contra-streamer"
wait_running "contra-indexer-solana"
wait_running "contra-indexer-contra"
wait_running "contra-operator-solana"
wait_running "contra-operator-contra"
wait_running "contra-prometheus"
wait_running "contra-grafana"
wait_running "contra-cadvisor"
wait_running "contra-blackbox-exporter"
wait_running "contra-pg-backup-primary"
wait_running "contra-pg-backup-indexer"

echo "All services stable."

# ---- Run bench ------------------------------------------------------------

echo ""
echo "Running bench on cores [${BENCH_CPUSET:-any}]..."
echo "-------------------------------------------------------"

if [ -n "${BENCH_CPUSET}" ]; then
    taskset -c "${BENCH_CPUSET}" "${BENCH_BIN}" \
        --admin-keypair "${ADMIN_KEYPAIR_FILE}" \
        --rpc-url "http://localhost:${GATEWAY_PORT}" \
        "${BENCH_ARGS[@]+"${BENCH_ARGS[@]}"}"
else
    "${BENCH_BIN}" \
        --admin-keypair "${ADMIN_KEYPAIR_FILE}" \
        --rpc-url "http://localhost:${GATEWAY_PORT}" \
        "${BENCH_ARGS[@]+"${BENCH_ARGS[@]}"}"
fi

echo "-------------------------------------------------------"
echo "Bench complete."
