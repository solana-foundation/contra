# Railway: shared `contra-app` image migration

This doc is a draft plan for replicating the local §1.6 pattern (one shared
`contra-app:${CONTRA_VERSION}` image, N Railway services that all reference it
with different start commands) on Railway. It is **not implemented**. Today
each Railway service builds its own copy of the same `Dockerfile` — wasteful
but functional.

Read [`RAILWAY.md`](RAILWAY.md) first for the current per-service-build flow.

---

## Why bother

| Today (per-service build) | Shared image |
|---|---|
| 9 Rust services × full Cargo build per deploy | 1 build, 9 image pulls |
| Services can drift if a deploy partially fails | All services byte-identical, guaranteed |
| Rollback = re-deploy from old commit (rebuild) | Rollback = re-pin to previous tag (seconds) |
| `CONTRA_VERSION` ignored | `CONTRA_VERSION` becomes the deploy primitive |
| `git push` triggers Railway rebuild | CI builds & pushes; Railway redeploys on tag change |

**Skip this if:** Railway is dev/staging where per-push auto-rebuild is more
valuable than build savings. **Do this if:** you deploy frequently, you need
schema-coupled services (gateway↔indexer) to flip atomically, or build minutes
are starting to hurt.

---

## Target architecture

```
┌──────────────┐    git push tag vX.Y.Z    ┌─────────────────────┐
│  GitHub      │ ────────────────────────▶ │  GitHub Actions     │
│  (this repo) │                           │  (build & push)     │
└──────────────┘                           └──────────┬──────────┘
                                                      │
                                                      ▼
                                           ┌─────────────────────┐
                                           │  GHCR registry      │
                                           │  ghcr.io/<org>/     │
                                           │  contra-app:vX.Y.Z  │
                                           └──────────┬──────────┘
                                                      │ pull
                            ┌─────────────────────────┼─────────────────────────┐
                            ▼                         ▼                         ▼
                  ┌──────────────┐          ┌──────────────┐          ┌──────────────┐
                  │ write-node   │          │ gateway      │   ...    │ operator-    │
                  │ Railway svc  │          │ Railway svc  │          │ contra svc   │
                  │              │          │              │          │              │
                  │ start cmd:   │          │ start cmd:   │          │ start cmd:   │
                  │ contra-node  │          │ gateway      │          │ indexer      │
                  └──────────────┘          └──────────────┘          └──────────────┘
```

Each Railway service is configured with **Source = Docker Image** (not GitHub
repo) and points at the same registry tag.

---

## Phase 1 — CI workflow (build & push)

Add `.github/workflows/release-image.yml`. Triggers on git tag push (e.g.
`v1.2.3`) and on manual dispatch.

```yaml
name: Build & push contra-app image

on:
  push:
    tags: ['v*']
  workflow_dispatch:
    inputs:
      tag:
        description: Image tag to publish (e.g. v1.2.3 or sha-abc1234)
        required: true

permissions:
  contents: read
  packages: write

env:
  REGISTRY: ghcr.io
  IMAGE_NAME: ${{ github.repository_owner }}/contra-app

jobs:
  build-and-push:
    runs-on: contra-runner-1
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - name: Resolve tag
        id: tag
        run: |
          if [ "${{ github.event_name }}" = "push" ]; then
            echo "tag=${GITHUB_REF_NAME}" >> "$GITHUB_OUTPUT"
          else
            echo "tag=${{ inputs.tag }}" >> "$GITHUB_OUTPUT"
          fi

      - name: Load pinned versions
        run: |
          set -a; . versions.env; set +a
          echo "SOLANA_VERSION=$SOLANA_VERSION" >> "$GITHUB_ENV"
          echo "PNPM_VERSION=$PNPM_VERSION"   >> "$GITHUB_ENV"

      - name: Set up QEMU
        uses: docker/setup-qemu-action@v3

      - name: Set up Buildx
        uses: docker/setup-buildx-action@v3

      - name: Log in to GHCR
        uses: docker/login-action@v3
        with:
          registry: ${{ env.REGISTRY }}
          username: ${{ github.actor }}
          password: ${{ secrets.GITHUB_TOKEN }}

      - name: Build & push
        uses: docker/build-push-action@v6
        with:
          context: .
          file: Dockerfile
          platforms: linux/amd64
          push: true
          build-args: |
            SOLANA_VERSION=${{ env.SOLANA_VERSION }}
            PNPM_VERSION=${{ env.PNPM_VERSION }}
          tags: |
            ${{ env.REGISTRY }}/${{ env.IMAGE_NAME }}:${{ steps.tag.outputs.tag }}
            ${{ env.REGISTRY }}/${{ env.IMAGE_NAME }}:latest
          cache-from: type=registry,ref=${{ env.REGISTRY }}/${{ env.IMAGE_NAME }}:buildcache
          cache-to: type=registry,ref=${{ env.REGISTRY }}/${{ env.IMAGE_NAME }}:buildcache,mode=max
```

**Notes:**
- `cache-from`/`cache-to` use the registry as a remote BuildKit cache, so the
  second tag-build is fast. The `:buildcache` tag is just metadata, not a
  runnable image.
- `latest` floats to the most recent tag. Railway services pin to a specific
  `vX.Y.Z` (see Phase 2) — `latest` is for ad-hoc ops only.
- The Dockerfile already accepts `SOLANA_VERSION` / `PNPM_VERSION` as build
  args; no Dockerfile change needed.
- Image visibility: GHCR packages default to private. After the first push,
  go to GitHub → Packages → contra-app → Settings → set to private and add
  Railway as a collaborator (see Phase 2 secrets).

---

## Phase 2 — Railway service reconfiguration

For each of the 8 Rust services (`write-node`, `read-node`, `gateway`,
`streamer`, `indexer-solana`, `indexer-contra`, `operator-solana`,
`operator-contra`):

1. **Settings → Source** → switch from "GitHub repo" to "Docker Image".
2. **Image:** `ghcr.io/<org>/contra-app:v1.2.3` (start with the first tag CI
   produces). Bump this to redeploy.
3. **Registry credentials:** Settings → Variables → add as service-scoped:
   - `RAILWAY_DOCKERFILE_PATH` — remove if previously set.
   - GHCR pull secret: in Railway, go to Project → Settings → Tokens, add
     a GitHub PAT with `read:packages`. Reference it as the registry auth
     for any private image.
4. **Start command:** unchanged from current RAILWAY.md (e.g.
   `/usr/local/bin/contra-node` for `write-node`).
5. **Environment variables:** unchanged.
6. **Health check / port / domain:** unchanged.

`admin-ui` stays on its own image (`Dockerfile` in `admin-ui/`), since it's
not part of the Rust binary set. Same for `blackbox-exporter` (uses
`prom/blackbox-exporter:${BLACKBOX_VERSION}` directly).

---

## Phase 3 — promotion / rollback flow

**Promote a new version:**

```bash
git tag v1.2.4
git push origin v1.2.4
# wait for the release-image workflow to finish (~10–15 min cold, ~2–4 min warm)

# Then in Railway, for each Rust service:
#   Settings → Source → Image → bump to v1.2.4
# Or scripted via Railway CLI:
railway service update --image ghcr.io/<org>/contra-app:v1.2.4 \
  --service write-node \
  --service read-node \
  --service gateway \
  --service streamer \
  --service indexer-solana \
  --service indexer-contra \
  --service operator-solana \
  --service operator-contra
```

**Rollback:**

```bash
# Pin all services back to the previous tag — no rebuild needed.
railway service update --image ghcr.io/<org>/contra-app:v1.2.3 \
  --service write-node ...
```

Rollback latency is the time it takes Railway to pull and start the image
(~30–90 seconds), versus a full rebuild (~10+ min) under the per-service
flow.

---

## Phase 4 — optional: auto-redeploy on tag

Railway can poll an image tag and redeploy when its digest changes. To wire
this up:

1. Use a moving tag like `staging` or `production` (not the immutable
   `vX.Y.Z`).
2. Have CI re-tag after a successful build:
   ```yaml
   - name: Promote to staging
     if: success() && github.ref == 'refs/heads/main'
     run: |
       docker buildx imagetools create \
         -t ghcr.io/<org>/contra-app:staging \
         ghcr.io/<org>/contra-app:${{ steps.tag.outputs.tag }}
   ```
3. In Railway, point each service at `:staging` and enable
   **Settings → Source → Watch image** (polls every ~5 min).

This trades atomic deploys for ergonomics: services may briefly run mixed
versions during the window when some have pulled the new digest and others
haven't.

---

## Migration checklist

- [ ] Create GHCR access token, add to repo secrets if needed beyond
      `GITHUB_TOKEN`.
- [ ] Land `release-image.yml` workflow.
- [ ] Push a test tag (`v0.0.0-rc1`) and verify the image lands in GHCR.
- [ ] Pull the image locally and smoke-test one binary
      (`docker run --rm ghcr.io/<org>/contra-app:v0.0.0-rc1 gateway --help`).
- [ ] Reconfigure **one** non-critical Railway service (e.g. `streamer`)
      to pull from the registry; verify deploy works end-to-end.
- [ ] Reconfigure remaining 7 Rust services.
- [ ] Decommission per-service GitHub auto-build triggers.
- [ ] Document the new promote/rollback flow in `RAILWAY.md`.

---

## Open questions

- **GHCR cost:** free for public repos and within GitHub Actions egress
  limits. Private repos with high pull volume may incur fees — confirm
  before committing.
- **Build cache invalidation across runners:** the registry cache works
  across self-hosted and GitHub-hosted runners, but cold cache on a new
  runner means a full rebuild. Pin to one runner pool initially.
- **Multi-arch:** the workflow above is `linux/amd64` only. If Railway
  ever moves to ARM nodes, add `linux/arm64` to `platforms` (cost: ~2× build
  time, since cross-compile via QEMU is slow).
- **Schema migrations:** atomic image promotion does NOT atomically migrate
  databases. If a tag includes schema changes, run migrations as a separate
  step before promoting (or use a migration container on a separate Railway
  service).
