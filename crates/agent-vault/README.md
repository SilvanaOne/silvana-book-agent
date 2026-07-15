# agent-vault

**Catalog entry + interactive demo.** The Rust/production source for Silvana Vault
lives in a **separate proprietary repository** (`silvana-vault/`, sibling of this
repo). This crate carries the catalog metadata (`agent.toml`), the docs
(`doc.md`), and a **Next.js UI mirror** in `demo/` that lets a user click through
the vault's actual flow (register → dashboard → Load keys / Balances) without
running the real backend.

## What Silvana Vault is

Multi-tenant secrets vault that lets any Silvana agent (open-core) delegate
sensitive key material to a hardened, browser-encrypted store:

- **Canton wallets** — Ed25519 private key or BIP39 seed.
- **CEX API keys** — Bybit, KuCoin.
- Browser-side **RSA-OAEP-SHA256** encryption per tenant; server holds ciphertext only.
- **HMAC API** with a `tenantUserId` on every route — the same contract as
  `schemas/proprietary-engine-api.yaml`.

## How agents use it

1. Tenant registers at `https://vault.spcatcher.cfd/register`, receives an
   **Agent User ID** (UUID).
2. On the agent side, set `SIGNER_TENANT_USER_ID=<uuid>`, `SIGNER_API_URL`,
   `SIGNER_API_TOKEN`.
3. On first `POST /agent/heartbeat`, the vault orchestrates a per-tenant
   executor at `https://exec-{shortId}.exec-infra.spcatcher.cfd`.
4. That executor boots and asks Vault for a scoped **runtime bundle**
   (`POST /runtime/bundle`) — decrypted secrets are held **only in the executor
   process memory**, never in open-core.

Open-core never sees plaintext CEX keys.

## Demo (this repo)

The `demo/` directory is a self-contained Next.js app that reproduces the
vault's UI (login, register, verify-email, dashboard with **Load keys** and
**Balances** tabs) with an in-memory backend. Nothing is persisted, no real
crypto material leaves the browser, and any credentials pass the login form.
It exists so that the vault has a clickable preview inside the standard demo
grid without pointing users at the real hosted service.

Run locally:

```bash
cd crates/agent-vault/demo
npm install
npm run dev
# open http://localhost:3088
```

Docker image (matches CI):

```bash
docker build -t agent-vault-demo crates/agent-vault/demo
docker run --rm -p 3088:3088 agent-vault-demo
```

Real deployment: <https://vault.spcatcher.cfd> (proprietary; source in
`silvana-vault/`).

## Links

- Live vault (staging): <https://vault.spcatcher.cfd>
- Target production URL: <https://vault.silvana.one>
- Ecosystem doc: `agent-arbitrage`'s `.env.example` — `SIGNER_*` block for the
  agent-side wiring (`EXECUTOR_API_URL`, `SIGNER_TENANT_USER_ID`, …).
