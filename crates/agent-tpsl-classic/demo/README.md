# agent-tpsl-classic demo

Interactive playground for the [`agent-tpsl-classic`](..) Rust agent. Simulate a
Take-Profit / Stop-Loss position with a synthetic price walk and watch
triggers fire in real time — no real orders, no on-chain calls.

## Quick start

```bash
cd crates/agent-tpsl-classic/demo
npm install
npm run dev
```

Open <http://localhost:3002>.

## What it shows

- **Position setup** — configure market, side (long/short), quantity,
  entry price, TP, SL, optional trailing SL percentage.
- **Status panel** — live current price, unrealized PnL, entry / peak
  or trough / TP / SL (trailing SL moves as peak advances).
- **Price chart** — SVG line chart of the simulated price with
  horizontal lines for entry, TP, and current SL. When triggered, a
  colored marker pins the exact tick.
- **Events log** — every trailing-SL update and every TP/SL trigger
  timestamped and colored.
- **Demo tools** — nudge price ±0.5/2/5%, jump straight to TP or SL,
  set a manual price, or tune the random-walk drift / volatility.

## How it works

- `lib/tpsl-engine.ts` — TypeScript port of the trigger rules from
  `crates/agent-tpsl-classic/src/main.rs` (`fn tpsl_loop`). Same trailing-SL
  ratchet, same TP/SL semantics per side.
- `lib/price-simulator.ts` — cheap GBM random walk (Box-Muller).
- `lib/store.ts` — single-process in-memory state + a 1 Hz tick timer
  that pushes prices through the engine.
- `app/api/tpsl/{start,stop,reset,state}/route.ts` — REST endpoints
  driving the engine.
- `app/api/price/{jump,walk}/route.ts` — demo-only overrides for the
  price simulator.

## Not covered

- Multi-position (single position at a time).
- Persistence across restarts (in-memory only).
- Auth / multi-user (demo is intended for a single operator).

## Wiring to real orderbook

If you want to point the same UI at the real `agent-tpsl-classic` binary,
replace `lib/store.ts`'s in-memory engine with a poll of
`http://…/api/state` from a running Rust agent, and route the "start" /
"stop" buttons to a supervisor that spawns / kills the binary.
