# contra-auth

Authentication service for the Contra platform. Handles user registration, login, and Solana wallet verification. Issues JWTs consumed by the gateway for RBAC enforcement.

## Configuration

| Variable | Default | Description |
|---|---|---|
| `AUTH_PORT` | `8903` | Port to listen on |
| `AUTH_DATABASE_URL` | — | Postgres connection URL |
| `JWT_SECRET` | — | HS256 signing secret. Must match the gateway's `JWT_SECRET`. |
| `CORS_ALLOWED_ORIGIN` | `*` | Value for `Access-Control-Allow-Origin`. Set to your frontend origin in production (e.g. `https://app.contra.xyz`). Defaults to `*` for local dev. |

## API

All endpoints are under `/auth`.

### `POST /auth/register`

Create a new account. All users are registered with the `user` role.

```json
{ "username": "alice", "password": "hunter2" }
```

Password requirements: minimum 6 characters, maximum 72 characters (Argon2's input limit — inputs beyond 72 bytes are silently truncated, so longer passwords are rejected outright).

Returns the created user. Passwords are hashed with Argon2 and never returned.

---

### `POST /auth/login`

Authenticate and receive a signed JWT (valid for 24 hours).

```json
{ "username": "alice", "password": "hunter2" }
```

Returns `{ "token": "<jwt>" }`. Both wrong username and wrong password return `401` to prevent username enumeration.

---

### `POST /auth/challenge-wallet` 🔒

Request a sign challenge to prove ownership of a Solana wallet. Requires a valid JWT.

Returns a `message`, `nonce`, and `expires_at`. The challenge expires in 10 minutes.

```json
{
  "message": "Contra wallet verification\nuser: <uuid>\nnonce: <uuid>\nexpires: <unix>",
  "nonce": "<uuid>",
  "expires_at": "<iso8601>"
}
```

---

### `POST /auth/verify-wallet` 🔒

Submit the signed challenge to register a wallet as verified. Requires a valid JWT.

```json
{
  "pubkey": "<base58 pubkey>",
  "nonce": "<uuid from challenge>",
  "signature": "<base58 signature>"
}
```

The service reconstructs the exact challenge message, verifies the Ed25519 signature against the provided pubkey, then stores the wallet. Each nonce can only be consumed once — replays are rejected.

---

### `GET /auth/wallets` 🔒

List all verified wallets for the authenticated user. Requires a valid JWT.

---

### `GET /health`

Liveness check. Returns `200 ok`.

## Roles

There are two roles: `user` (default) and `operator`.

| Role | Description |
|---|---|
| `user` | Standard role. All registered accounts start as `user`. |
| `operator` | Elevated role. Can call operator-only methods on the gateway without ownership checks. |

**Operators must be provisioned directly in the database** — there is no API to assign or escalate to the operator role. This is intentional: operator access is an infrastructure-level concern, not a self-service one.

```sql
UPDATE contra_auth.users SET role = 'operator' WHERE username = 'alice';
```

## Wallet verification flow

Wallets are not trusted on assertion — the user must cryptographically prove they control the private key.

```
1. POST /auth/challenge-wallet
   ← { message, nonce, expires_at }

2. Sign `message` with the wallet's private key (Ed25519)

3. POST /auth/verify-wallet  { pubkey, nonce, signature }
   ← { pubkey, created_at }
```

Once verified, the gateway allows that user to query accounts owned or delegated by that wallet (ATAs, token accounts, etc.).

## JWT format

Tokens are signed with HS256. The payload contains:

```json
{
  "sub": "<user uuid>",
  "role": "user | operator",
  "iss": "contra-auth",
  "aud": "contra-gateway",
  "exp": <unix timestamp>
}
```

The gateway validates `iss`, `aud`, and `exp` on every request. A token issued by any other service, even with the same secret but missing these claims, will be rejected.

## Running tests

```
cargo test --test integration -- --test-threads=1
```

Tests spin up a real Postgres via Docker (testcontainers). Docker must be running.
