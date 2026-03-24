#!/usr/bin/env bash
# =============================================================================
# run.sh — Full-stack contra bench-tps orchestration script
#
# What this script does (in order):
#   1.  Parse script-level flags (--rebuild, --clean).
#   2.  Sanity-check that the bench binary and .env file exist.
#   3.  Load environment variables from .env.
#   4.  Compute CPU affinity splits so services and the bench binary don't
#       compete for the same cores.
#   5.  Generate (or reuse) a persistent admin keypair and patch it into .env
#       so the write-node whitelists the admin for privileged transactions.
#   6.  Build Solana programs (.so files) if missing or --rebuild was passed.
#   7.  Build Docker service images if missing or --rebuild was passed.
#   8.  Optionally wipe data volumes (--clean) to start from a clean state.
#   9.  Fix WAL archive volume permissions that Docker creates as root.
#   10. Start all Docker Compose services.
#   11. Pin all contra containers to the service CPU set.
#   12. Wait for every service to reach a stable/healthy state before
#       proceeding — this prevents the bench from hitting a half-started node.
#   13. Run the bench binary (with optional CPU pinning) and forward any
#       extra CLI arguments passed to this script.
#   14. Stop all services on exit (via trap).
# =============================================================================
set -euo pipefail

# ---------------------------------------------------------------------------
# Path setup
# ---------------------------------------------------------------------------

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BENCH_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
REPO_ROOT="$(cd "${BENCH_DIR}/.." && pwd)"

# Release binary produced by: cargo build --release --manifest-path bench-tps/Cargo.toml
BENCH_BIN="${BENCH_DIR}/target/release/contra-bench-tps"

# .env file loaded by both this script and docker compose.
# Copy from .env.sample and fill in values before first run.
BENCH_ENV="${BENCH_DIR}/.env"

# ---------------------------------------------------------------------------
# Step 1 — Parse script-level flags
#
# --rebuild  Force-rebuild the Rust binary, Solana programs, and Docker images.
#            Also regenerates the admin keypair.  Use this after code changes.
#
# --clean    Wipe all Docker volumes (postgres data, validator ledger) before
#            starting.  Use this when a previous run was interrupted and left
#            corrupt state (e.g. "unexpected end of file" errors on startup).
#
# Any other flags are collected into BENCH_ARGS and forwarded verbatim to the
# bench binary at the end of the script (e.g. --threads 20 --duration 120).
# ---------------------------------------------------------------------------
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

# ---------------------------------------------------------------------------
# Step 2 — Sanity checks
#
# The binary must be compiled before running this script.  The .env file
# must exist (copy .env.sample and fill in values).
# ---------------------------------------------------------------------------
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

# ---------------------------------------------------------------------------
# Step 3 — Load environment variables from .env
#
# set -a exports every variable defined while the file is sourced so that
# child processes (docker compose, the bench binary) inherit them.
# ---------------------------------------------------------------------------
# shellcheck disable=SC1091
set -a; source "${BENCH_ENV}"; set +a

WRITE_PORT="${CONTRA_WRITE_PORT:-8899}"
GATEWAY_PORT="${GATEWAY_PORT:-8898}"

# ---------------------------------------------------------------------------
# Step 4 — CPU affinity split (75% services / 25% bench)
#
# Assigning separate cores to Docker services and the bench binary eliminates
# CPU competition that would artificially inflate RTT measurements.
#
# Layout on an 8-core machine:
#   cores 0-5  → all contra Docker containers (write-node, postgres, etc.)
#   cores 6-7  → bench binary (contra-bench-tps)
#
# On a single-core machine this is skipped entirely.
# ---------------------------------------------------------------------------
TOTAL_CORES=$(nproc)

if [ "${TOTAL_CORES}" -lt 2 ]; then
    echo "WARNING: only ${TOTAL_CORES} core(s) detected — skipping CPU pinning" >&2
    SERVICE_CPUSET=""
    BENCH_CPUSET=""
else
    # Allocate 75% of cores (rounded down, minimum 1) to services.
    SERVICE_COUNT=$(( TOTAL_CORES * 3 / 4 ))
    [ "${SERVICE_COUNT}" -lt 1 ] && SERVICE_COUNT=1
    # Reserve at least one core for the bench binary.
    [ "${SERVICE_COUNT}" -ge "${TOTAL_CORES}" ] && SERVICE_COUNT=$(( TOTAL_CORES - 1 ))

    BENCH_START="${SERVICE_COUNT}"
    BENCH_END=$(( TOTAL_CORES - 1 ))

    # cpuset strings accepted by taskset -c and docker update --cpuset-cpus.
    SERVICE_CPUSET="0-$(( SERVICE_COUNT - 1 ))"
    BENCH_CPUSET="${BENCH_START}-${BENCH_END}"

    echo "CPUs: total=${TOTAL_CORES}  services=[${SERVICE_CPUSET}]  bench=[${BENCH_CPUSET}]"
fi

# ---------------------------------------------------------------------------
# Step 5 — Build / reuse admin keypair and patch .env
#
# The admin keypair serves two purposes:
#   a. The bench binary uses it to initialise the SPL mint, create ATAs, and
#      mint initial token balances to every account (setup phase).
#   b. The write-node reads CONTRA_ADMIN_KEYS at startup to decide which
#      public keys are allowed to submit privileged admin transactions.
#
# To avoid a chicken-and-egg problem the keypair is generated here, before
# any Docker service starts, and the two env vars are patched into .env:
#   CONTRA_ADMIN_KEYS   — space-separated base58 public key(s)
#   ADMIN_PRIVATE_KEY   — the full private key JSON, one line (no whitespace)
#
# The keypair file is reused across runs so the write-node config remains
# valid without needing a full restart.  Pass --rebuild to regenerate it.
# ---------------------------------------------------------------------------
if ! command -v solana-keygen > /dev/null 2>&1; then
    echo "ERROR: solana-keygen not found in PATH" >&2
    echo "       Install the Solana CLI or add it to PATH before running run.sh" >&2
    exit 1
fi

ADMIN_KEYPAIR_FILE="${BENCH_DIR}/admin-keypair.json"

if [ "${REBUILD}" -eq 1 ] || [ ! -f "${ADMIN_KEYPAIR_FILE}" ]; then
    # --force overwrites an existing file without prompting.
    # --no-bip39-passphrase / --silent suppress interactive prompts.
    solana-keygen new --no-bip39-passphrase --silent --force --outfile "${ADMIN_KEYPAIR_FILE}"
    echo "Generated admin keypair: ${ADMIN_KEYPAIR_FILE}"
else
    echo "Reusing existing admin keypair: ${ADMIN_KEYPAIR_FILE}"
fi

ADMIN_PUBKEY=$(solana-keygen pubkey "${ADMIN_KEYPAIR_FILE}")
# Strip all whitespace from the JSON bytes array so it fits on a single line
# and can be embedded as an env var value without quoting issues.
ADMIN_PRIVKEY_JSON=$(tr -d '[:space:]' < "${ADMIN_KEYPAIR_FILE}")

echo "Admin pubkey: ${ADMIN_PUBKEY}"

# patch_env KEY VALUE — updates or appends a KEY=VALUE line in .env in-place.
# Using sed with the | delimiter avoids breakage if VALUE contains slashes.
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

# Re-source .env so the rest of this script sees the updated values (docker
# compose also re-reads the file at startup, but explicit variables in the
# shell environment take precedence).
# shellcheck disable=SC1091
set -a; source "${BENCH_ENV}"; set +a

# ---------------------------------------------------------------------------
# Step 6 — Build Solana programs (.so files)
#
# The solana-test-validator mounts the compiled program .so files at startup
# via --bpf-program flags in docker-compose.yml.  If the files are missing
# the validator will fail to start.
#
# Programs are compiled with Anchor.  Build times are 3–10 minutes on first
# run; subsequent builds are cached by Cargo.
# ---------------------------------------------------------------------------
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

# ---------------------------------------------------------------------------
# Step 7 — Build Docker service images
#
# Docker Compose project name defaults to the repo directory name ("contra"),
# so images are tagged contra-<service>.  All service images are checked as a
# group: if any is missing the entire set is rebuilt to ensure consistency.
#
# The COMPOSE array is built as an array (not a string) to safely handle paths
# that might contain spaces.
# ---------------------------------------------------------------------------
COMPOSE=(docker compose -f "${REPO_ROOT}/docker-compose.yml" --env-file "${BENCH_ENV}")

BUILT_IMAGES=(contra-write-node contra-read-node contra-gateway contra-streamer contra-activity contra-validator contra-indexer-solana contra-indexer-contra contra-operator-solana contra-operator-contra contra-prometheus)
BUILT_SERVICES=(write-node read-node gateway streamer activity validator indexer-solana indexer-contra operator-solana operator-contra prometheus)

images_exist() {
    # docker image inspect exits non-zero if any image in the list is missing.
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

# ---------------------------------------------------------------------------
# Step 8 — Optionally wipe data volumes (--clean)
#
# postgres-primary, postgres-replica, postgres-indexer, and the validator
# ledger are all stored in named Docker volumes.  If a previous run was
# interrupted mid-write (e.g. Ctrl+C during a DB migration) the volumes may
# contain corrupt data.  Symptoms: "unexpected end of file", WAL errors, or
# the write-node failing to start.
#
# --clean calls `docker compose down -v` which removes all named volumes
# associated with this compose project.  The next startup will reinitialise
# from scratch (migrations re-run, validator resets its ledger, etc.).
#
# WARNING: this permanently deletes all data in those volumes.
# ---------------------------------------------------------------------------
if [ "${CLEAN}" -eq 1 ]; then
    echo "Removing data volumes (--clean flag set)..."
    "${COMPOSE[@]}" down -v 2>/dev/null || true
    echo "Data volumes removed."
fi

# ---------------------------------------------------------------------------
# Step 9 — Fix WAL archive volume permissions
#
# Docker creates named volumes owned by root.  The postgres containers run as
# the "postgres" user (uid 70 on Alpine), which cannot write to a root-owned
# directory.  We fix ownership by running a one-off alpine container that
# mounts each volume and chowns it before the postgres containers start.
#
# If a volume does not yet exist (first run after --clean) the chown still
# succeeds because Docker auto-creates the volume when the container starts.
# ---------------------------------------------------------------------------
for vol in postgres-indexer-wal-archive postgres-primary-wal-archive; do
    docker run --rm -v "${vol}:/vol" postgres:16-alpine \
        chown postgres:postgres /vol 2>/dev/null \
        && echo "Fixed permissions on volume ${vol}" \
        || echo "WARNING: could not fix permissions on ${vol} (may not exist yet)"
done

# ---------------------------------------------------------------------------
# Step 10 — Start all services
#
# --no-build skips Docker's build-context re-evaluation (we already built
# above) which makes startup faster and avoids redundant layer checks.
# Services run in detached mode (-d); logs are not tailed here.
# ---------------------------------------------------------------------------
echo "Starting all services..."
"${COMPOSE[@]}" up -d --no-build

# ---------------------------------------------------------------------------
# Step 11 — Pin contra containers to service CPU cores
#
# docker update --cpuset-cpus applies cgroup CPU affinity after a container
# has already started.  We query running containers by name prefix rather than
# hardcoding a list so new services are pinned automatically.
# ---------------------------------------------------------------------------
if [ -n "${SERVICE_CPUSET}" ]; then
    echo "Pinning containers to cores [${SERVICE_CPUSET}]..."
    while IFS= read -r container; do
        docker update --cpuset-cpus="${SERVICE_CPUSET}" "${container}" 2>/dev/null \
            && echo "  pinned ${container}" \
            || echo "  WARNING: could not pin ${container}"
    done < <(docker ps --filter "name=contra-" --format "{{.Names}}")
fi

# ---------------------------------------------------------------------------
# Step 12 — Wait for every service to reach a stable state
#
# Three wait strategies are used depending on what each service exposes:
#
#   wait_healthy   — polls Docker's built-in healthcheck status (requires a
#                    HEALTHCHECK in the Dockerfile).  Used for postgres
#                    instances (pg_isready) and the validator (cluster-version).
#
#   wait_rpc       — sends a getLatestBlockhash JSON-RPC request and waits for
#                    an HTTP 200 response.  Used for write-node and read-node
#                    because it proves the full path (DB → migration → RPC) is
#                    live, not just that the process started.  Fails fast if
#                    the container crashes rather than waiting the full timeout.
#
#   wait_http_health — sends a GET request to a /health HTTP endpoint.  Used
#                    for the gateway which exposes its own health check.
#
#   wait_running   — checks that the container State.Status = "running".  Used
#                    for services that have no healthcheck or RPC endpoint
#                    (streamer, indexers, operators, observability stack).
#
# All waits poll every 2 seconds and print a dot per attempt so the operator
# can see progress.  Services with a fixed maximum wait time will error out
# and print the docker logs command to use for debugging.
# ---------------------------------------------------------------------------

# --- wait helper functions -------------------------------------------------

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
# Succeeds as soon as the node responds to getLatestBlockhash (any HTTP 200).
# Also fails fast if the container exits or crashes during the wait.
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
        # Fail fast rather than waiting the full timeout if the container died.
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

# Waits for a container to reach the "running" state (no Docker healthcheck).
# Fails fast if the container exits or crashes.
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

# Waits for an HTTP endpoint to return any successful response.
# Used for the gateway's /health endpoint which checks the gateway process
# itself, not a proxied backend — so it responds independently of write/read
# node availability.
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

# --- Wait for each service group in dependency order ----------------------

# Databases must be healthy before write-node/read-node attempt migrations.
wait_healthy "contra-postgres-primary"
wait_healthy "contra-postgres-replica"
wait_healthy "contra-postgres-indexer"

# Validator must be healthy (confirmed via solana cluster-version healthcheck)
# before write-node and read-node attempt to connect to it.
wait_healthy "contra-validator"

# Write-node and read-node: probe via JSON-RPC to confirm the full startup path
# (DB connection, schema migration, RPC listener) is complete.
wait_rpc "write-node" "http://localhost:${WRITE_PORT}" "contra-write-node"
wait_rpc "read-node"  "http://localhost:${READ_PORT}"  "contra-read-node"

# Gateway: check its own /health endpoint (not a proxied backend call).
wait_http_health "gateway" "http://localhost:${GATEWAY_PORT}/health"

# Remaining services have no healthcheck or RPC endpoint; just confirm they
# are running and haven't crashed immediately on startup.
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

# ---------------------------------------------------------------------------
# Step 13 — Run the bench binary
#
# Two mandatory arguments are always injected by this script:
#   --admin-keypair  path to the keypair generated in step 5
#   --rpc-url        gateway address (bench sends through gateway so that
#                    reads are routed to the read-node automatically)
#
# Any extra arguments passed to run.sh (i.e. those not consumed in step 1)
# are forwarded verbatim, allowing callers to override defaults:
#   ./scripts/run.sh --threads 20 --duration 120 --num-conflict-groups 1
#
# When CPU pinning is active, taskset -c restricts the bench process to the
# bench CPU set so it does not compete with service containers.
# ---------------------------------------------------------------------------

# Register a cleanup function that stops all Docker services when this script
# exits (normally, on error, or on Ctrl+C / SIGTERM).
cleanup() {
    echo ""
    echo "Stopping all services..."
    "${COMPOSE[@]}" stop 2>/dev/null || true
    echo "Done."
}
trap cleanup EXIT

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
