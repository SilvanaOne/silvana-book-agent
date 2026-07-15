# Silvana Vault

> Multi-tenant secrets vault for Silvana agents — browser-side RSA-OAEP encryption of Canton wallet keys and CEX (Bybit / KuCoin) API keys, HMAC-scoped runtime bundles for per-tenant executors, no plaintext to open-core.

## What it does

Silvana Vault stores private key material for every tenant that wants to run a
Silvana-hosted agent, and hands scoped runtime bundles to per-tenant executor
containers when the agent goes live. It is the single source of truth for
CEX API keys (Bybit, KuCoin) and Canton wallet keys used by proprietary
execution engines.

Vault **does not trade**. It:

1. Stores and decrypts tenant keys (browser → RSA-OAEP → server ciphertext).
2. Accepts heartbeats from open-core agents and orchestrates the provisioning
   of a per-tenant executor via Executor Infra.
3. Serves the scoped **runtime bundle** to the executor at boot — never to the
   open-core agent.

## Multi-tenant model (Model B)

1. User registers at `/register` (email + password; verification code when
   `AUTH_REGISTRATION_MODE=email_verification`, instant session when `instant`).
2. After login, the UI shows **Agent User ID** (Postgres UUID).
3. User encrypts keys in-browser (RSA-OAEP-SHA256 per tenant) and uploads
   **ciphertext only**.
4. User runs the open-core agent with `SIGNER_TENANT_USER_ID=<uuid>` +
   `SIGNER_API_URL` + `SIGNER_API_TOKEN`.
5. On first `POST /agent/heartbeat`, Vault asks Executor Infra to spin up a
   dedicated executor container at
   `https://exec-{shortId}.exec-infra.spcatcher.cfd`.
6. That executor calls `POST /runtime/bundle { engine, tenantUserId }` at boot
   and receives scoped decrypted secrets — held only in RAM.

Platform operator uses the env admin (`SIGNER_UI_USER` / `SIGNER_UI_PASS`) —
cannot load tenant keys; manages the service only.

## Scope

| Asset class | Supported |
|---|---|
| CEX | Bybit, KuCoin |
| Wallets | Canton (Ed25519 private key or BIP39 seed) |
| Balances tab | CEX via read-keys; Canton party-id cards |

## HMAC API

All routes require `tenantUserId` (UUID):

- `GET  /pubkey?tenantUserId=…`
- `POST /secrets/cex`        — `{ tenantUserId, venue, keyId, ciphertext, … }`
- `POST /secrets/wallet`     — `{ tenantUserId, chain, ciphertext, … }`
- `GET  /secrets/status?tenantUserId=…`
- `POST /runtime/bundle`     — `{ engine, tenantUserId }` (executor-only)
- `POST /agent/heartbeat`    — `{ tenantUserId, agentBuild, … }`

UI (session cookie): `/`, `/login`, `/register`, `/verify-email`,
`/api/auth/*`, `/ui/*`. The Next.js demo in this repo (`demo/`) mirrors the
same pages but serves its data under `/api/vault/*` instead of `/ui/*`.

## Agent-side wiring

In the open-core agent's `.env`:

```dotenv
SIGNER_API_URL=https://vault.spcatcher.cfd
SIGNER_API_TOKEN=<shared platform HMAC secret>
SIGNER_TENANT_USER_ID=<agent user id from vault UI>
AGENT_HEARTBEAT_MS=120000
```

In `STRATEGY_MODE=paper` the agent still emits heartbeats but does not forward
opportunities. Switch to `live` and provide `EXECUTOR_API_URL` (or leave it
unset — the vault will bootstrap a managed executor on first heartbeat).

## Domains

| Purpose | Staging (now) | Production (target) |
|---|---|---|
| Vault UI + HMAC API | `vault.spcatcher.cfd` | `vault.silvana.one` |
| Per-tenant executor suffix | `exec-infra.spcatcher.cfd` | `exec-infra.silvana.one` |
| Executor URL | `https://exec-{shortId}.exec-infra.spcatcher.cfd` | `https://exec-{shortId}.exec-infra.silvana.one` |

Switch profile via `SILVANA_DOMAIN_PROFILE=production` on the vault side.

## Threat model (short)

- Server never sees plaintext user secrets — they are RSA-OAEP-encrypted in the
  tenant's browser with an ephemeral per-tenant key pair; the tenant's RSA
  private key is wrapped by `SIGNER_MASTER_KEY` (32-byte hex) and only used
  server-side to unwrap into a per-request runtime bundle.
- Open-core never receives plaintext CEX keys; only the executor container
  (per-tenant, no shared memory) receives them, and only in RAM.
- The platform admin account cannot load tenant keys — only manage the
  service (bind ports, start Postgres, register operators).

## Metadata

| Field | Value |
|---|---|
| Category | `tx_flow` |
| Agent slug | `vault` |
| Template | `Silvana Vault v1` (typescript) |
| Status | `preview` |
| Tags | `vault`, `keys`, `hmac`, `multi-tenant` |
| Required balance | None — hosted SaaS |
| Supported assets | Canton Ed25519 wallets, Bybit + KuCoin API keys |
| Supported projects | Silvana |

## Package

- Production source lives in the proprietary `silvana-vault/` repo — Express +
  Postgres + admin panel + executor-infra hooks. Not built from this
  repository.
- This crate carries the catalog metadata (`agent.toml`), the docs (this
  file), and a **Next.js UI mirror** in `demo/` that reproduces the vault's
  register → dashboard → **Load keys** / **Balances** flow with an in-memory
  backend. Nothing from the demo is persisted or wired to real crypto —
  it exists so the vault has a clickable preview inside the standard demo
  grid.

## Resources

- Live vault (staging): <https://vault.spcatcher.cfd>
- Target production URL: <https://vault.silvana.one>
- Local demo: `cd crates/agent-vault/demo && npm install && npm run dev` →
  <http://localhost:3088>
- Docker: `docker build -t agent-vault-demo crates/agent-vault/demo && \
  docker run --rm -p 3088:3088 agent-vault-demo`
- Contact: <https://silvana.one/contact>
