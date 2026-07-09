# Auth — JWT

> JWT generation, decode, and verification CLI — auto-login, smart login, and 2FA-style flows for Silvana agents. Wraps the shared JWT/signing helpers.

## What it does

CLI wrapper around the JWT helpers in `orderbook-agent-logic::auth`.

## How it works

Auth Agent — JWT generation/inspection CLI

Thin wrapper around the existing `auth.rs` helpers. Generates the
self-describing Ed25519 JWTs (RFC 8037) used by Silvana orderbook services
and decodes/verifies tokens for debugging.

Reads the private key from `PARTY_AGENT_PRIVATE_KEY` env (Base58, Solana-style)
or `--private-key`. The party id is taken from `PARTY_AGENT` env or `--party`.

## Usage

```bash
agent-auth-jwt generate --role trader --ttl-secs 3600              # full JWT details
agent-auth-jwt generate --bare                                     # token only (for piping)
agent-auth-jwt decode --jwt <token>                                # pretty-print header + claims
agent-auth-jwt verify --jwt <token>                                # verify signature via embedded jwk
agent-auth-jwt user-id --jwt <token>                               # extract `sub` claim
```

## Metadata

| Field | Value |
|---|---|
| Category | `tx_flow` |
| Agent slug | `auth` |
| Template | `JWT` (rust) |
| Status | `active` |
| Tags | `jwt`, `auth`, `cli` |
| Required balance | None — offline |
| Supported assets | N/A — auth-only |
| Supported projects | Silvana |

## Package

- Crate: `agent-auth-jwt`
- Binary: `agent-auth-jwt`

## Resources

- Live demo: <https://silvana.one/agents/demo/auth-jwt/>
- Contact Us: <https://silvana.one/contact>
