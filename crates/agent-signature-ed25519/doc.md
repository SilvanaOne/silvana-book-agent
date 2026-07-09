# Signature — Ed25519

> Off-chain Ed25519 signing CLI — reads the private key from env or flag and never leaks it. Supports raw signing, canonical envelope signing, and verify.

## What it does

CLI wrapper around `message-signing`. Reads the private key from `PARTY_AGENT_PRIVATE_KEY`
or `--private-key`; never sends it anywhere.

## How it works

Signature Agent — off-chain Ed25519 signing CLI

Thin wrapper around the existing `message-signing` crate and `auth.rs`
helpers. Performs Ed25519 sign/verify and the canonical
`ed25519-sha256-v1` scheme used by the Ledger gRPC service.

Reads the private key from `PARTY_AGENT_PRIVATE_KEY` (Base58, Solana-style)
or `--private-key` flag. The key never leaves this process.

## Usage

```bash
agent-signature-ed25519 public-key
agent-signature-ed25519 sign-raw --input "hello"
agent-signature-ed25519 sign-canonical --input-base64 <b64>          # ed25519-sha256-v1
agent-signature-ed25519 verify-raw --input "hello" --signature <b64> --public-key <b64url>
```

## Metadata

| Field | Value |
|---|---|
| Category | `tx_flow` |
| Agent slug | `signature` |
| Template | `Ed25519` (rust) |
| Status | `active` |
| Tags | `ed25519`, `signing`, `cli` |
| Required balance | None — offline |
| Supported assets | N/A — signing-only |
| Supported projects | Silvana |

## Package

- Crate: `agent-signature-ed25519`
- Binary: `agent-signature-ed25519`

## Resources

- Live demo: <https://silvana.one/agents/demo/signature-ed25519/>
- Contact Us: <https://silvana.one/contact>
