# Silvana Book Agent

Monorepo for Silvana orderbook agents — cloud-native Rust liquidity providers and trading bots on the Canton ledger.

## Architecture

All agents share a common core (`orderbook-agent-logic`) and differ only in business logic and configuration.
Each agent is a separate binary crate.

### Workspace Structure

```
silvana-book-agent/
├── CLAUDE.md
├── Cargo.toml              # Workspace root
├── Makefile                # Build targets
├── proto/                   # Shared gRPC service definitions
│   └── silvana/
│       ├── ledger/v1/ledger.proto
│       ├── orderbook/v1/orderbook.proto
│       ├── settlement/v1/settlement.proto
│       └── pricing/v1/pricing.proto
└── crates/
    ├── orderbook-proto/         # Protobuf codegen (shared)
    ├── orderbook-agent-logic/   # Shared agent logic (settlement, orders, signing, auth, config, ...)
    ├── message-signing/         # Ed25519 canonical message signing
    ├── tx-verifier/             # Canton transaction verification + hashing
    ├── orderbook-cloud-agent/   # Orderbook liquidity agent (grid + RFQ)
    ├── agent-spot-dca/          # Spot DCA agent
    ├── agent-tpsl/              # Automated TP/SL agent
    ├── agent-watchlist/         # Read-only market watcher (prices + depth)
    ├── agent-order-expiry/      # Cancel stale orders by TTL
    ├── agent-state-monitor/     # Observe own orders + settlements (read-only)
    ├── agent-signature/         # Ed25519 sign/verify CLI
    ├── agent-auth/              # JWT generation/decode/verify CLI
    ├── agent-dca-portfolio/     # DCA across multiple markets in parallel
    ├── agent-notifier/          # Stream events → webhook / log file / stdout
    ├── agent-cash-buffer/       # Keep CC balance within a band via TransferCc
    ├── agent-killswitch/        # Emergency cancel-all + health monitor
    ├── agent-twap/              # Time-weighted average execution (slice large orders)
    ├── agent-spot-grid/         # Minimal grid market-maker (no RFQ)
    ├── agent-mean-reversion/    # EMA-based reversion trading
    ├── agent-risk-alert/        # Threshold breaches → webhook/log (no cancel)
    ├── agent-circuit-breaker/   # Price deviation → cancel-all + pause
    ├── agent-test-run/          # Pre-flight RPC healthcheck
    ├── agent-readiness-check/   # Pre-trade gate: balances/preapprovals/settlements
    ├── agent-portfolio-health/  # Aggregated balance + exposure report
    ├── agent-orderbook-streaming/ # Depth + prices → JSONL sinks
    ├── agent-oracle/            # Scheduled price publisher
    ├── agent-fair-value/        # Multi-source price aggregation
    ├── agent-spread-capture/    # Single-level two-sided MM with inventory clamp
    ├── agent-signal-bot/        # JSONL signal file → order execution
    ├── agent-inventory-mgmt/    # Keep instrument balance inside a target band
    ├── agent-order-matching/    # Snipe orders when best bid/offer crosses a trigger
    ├── agent-witnesses/         # Event-driven external command launcher
    ├── agent-failure-recovery/  # Watchdog for stale/failed settlements
    ├── agent-infinite-grid/     # Grid that follows mid (no fixed range)
    ├── agent-trend-analysis/    # SMA/EMA/Bollinger/RSI/MACD publisher (Tier 3)
    ├── agent-pairs-trading/     # Dual-market ratio divergence (Tier 3)
    ├── agent-iceberg-execution/ # Fill-driven small chunks of a large parent (Tier 3)
    ├── agent-volatility-screening/ # Rolling realized-vol publisher (Tier 3)
    ├── agent-pre-trade-check/   # Rule-engine order validator (Tier 3)
    ├── agent-pnl-screening/     # Realized/unrealized P&L tracker (Tier 3)
    ├── agent-trend-following/   # EMA crossover momentum trader (Tier 3)
    ├── agent-portfolio-rebalancing/ # Weight-based multi-asset rebalance (Tier 3)
    ├── agent-trading-history/   # Signed append-only event log (Tier 3)
    ├── agent-risk-exposure/     # Concentration + leverage proxy dashboard (Tier 3)
    ├── agent-blocked-party/     # Settlement-stream blocklist screener (Tier 3)
    ├── agent-hedging/           # Counter-position to push exposure to a target (Tier 3)
    ├── agent-block-execution/   # TWAP × Iceberg hybrid (Tier 3)
    ├── agent-batch-orders/      # Bulk submit / cancel CLI (Tier 3)
    ├── agent-compliance-screening/ # Rule-engine for settlement flows (Tier 3)
    ├── agent-liquidity-screening/  # Depth + slippage analytics (Tier 3)
    ├── agent-target-allocation/    # Absolute-value target balancer (Tier 3)
    ├── agent-liquidity-seeking/    # Depth-adaptive execution (Tier 3)
    ├── agent-selective-disclosure/ # Filtered + re-signed audit log (Tier 3)
    ├── agent-market-abuse/         # Spoof/layering self-audit (Tier 3)
    ├── agent-human-approval/       # Approval queue + sign-off CLI (Tier 3)
    ├── agent-concentration-risk/   # Cancel-on-breach enforcer (Tier 3)
    ├── agent-yield-rotation/       # Market carry ranking + rotation signal (Tier 3)
    ├── agent-treasury-mgmt/        # Policy + approval-routed rebalancer (Tier 3)
    ├── agent-contractual-compliance/ # Bilateral contract obligations (Tier 3)
    ├── agent-legal-compliance/     # Jurisdiction rule evaluator (Tier 3)
    ├── agent-scam-screening/       # Categorized threat-feed screener (Tier 3)
    ├── agent-risk-management/      # Composite live-state limits + enforce (Tier 3)
    ├── agent-inventory-risk/       # Soft/hard band signal + auto-hedge (Tier 3)
    ├── agent-smart-allocation/     # Two-level bucket portfolio (Tier 3)
    ├── agent-algo-order/           # Pluggable execution dispatcher (Tier 3)
    ├── agent-audit-attestation/    # Signed chain-head checkpoints (Tier 3)
    ├── agent-audit-replay/         # Offline policy back-test on trading-history (Tier 3)
    ├── agent-audit-retention/      # Chained per-day/week log rotation (Tier 3)
    └── agent-audit-anomaly/        # Post-hoc statistical anomaly scan (Tier 3)
```

### Crate Dependency Graph

```
orderbook-proto          (protobuf codegen)
    ↑
orderbook-agent-logic    (shared logic: runner, settlement, orders, liquidity, config, auth, state)
    ↑
message-signing          (Ed25519 signing)
tx-verifier              (Canton tx verification)
    ↑
├── orderbook-cloud-agent  (grid orders + RFQ + fill loops)
├── agent-spot-dca         (periodic DCA orders)
├── agent-tpsl             (take profit / stop loss)
├── agent-watchlist        (read-only market monitor — no ledger writes)
├── agent-order-expiry     (cancel orders older than TTL)
├── agent-state-monitor    (subscribe own orders + settlements)
├── agent-signature        (Ed25519 sign/verify CLI)
├── agent-auth             (JWT generation/decode/verify CLI)
├── agent-dca-portfolio    (multi-market DCA)
├── agent-notifier         (events → webhook / file / stdout sinks)
├── agent-cash-buffer      (rebalance CC via TransferCc)
├── agent-killswitch       (Tier 2: emergency cancel-all + health monitor)
├── agent-twap             (Tier 2: time-sliced large order execution)
├── agent-spot-grid        (Tier 2: minimal grid market-maker)
├── agent-mean-reversion   (Tier 2: EMA snap-back trading)
├── agent-risk-alert       (Tier 2: alert-only threshold monitor)
├── agent-circuit-breaker  (Tier 2: cancel-all on price deviation)
├── agent-test-run         (Tier 2: pre-flight RPC healthcheck)
├── agent-readiness-check  (Tier 2: pre-trade gate)
├── agent-portfolio-health (Tier 2: balance + exposure report)
├── agent-orderbook-streaming (Tier 2: market data fanout to JSONL sinks)
├── agent-oracle           (Tier 2: scheduled price publisher)
├── agent-fair-value       (Tier 2: multi-source price aggregation)
├── agent-spread-capture   (Tier 2: two-sided MM with inventory clamp)
├── agent-signal-bot       (Tier 2: JSONL signals → order execution)
├── agent-inventory-mgmt   (Tier 2: rebalance instrument inside a target band)
├── agent-order-matching   (Tier 2: snipe trades on trigger cross)
├── agent-witnesses        (Tier 2: events → exec external command)
├── agent-failure-recovery (Tier 2: stale/failed settlement watchdog)
├── agent-infinite-grid    (Tier 2: dynamic mid-following grid)
├── agent-trend-analysis   (Tier 3: TA indicators publisher)
├── agent-pairs-trading    (Tier 3: dual-market ratio divergence)
├── agent-iceberg-execution (Tier 3: fill-driven chunks of a large parent)
├── agent-volatility-screening (Tier 3: realized-vol publisher)
├── agent-pre-trade-check  (Tier 3: rule-engine validator, pure offline)
├── agent-pnl-screening    (Tier 3: realized/unrealized P&L via settlement stream)
├── agent-trend-following  (Tier 3: EMA crossover momentum)
├── agent-portfolio-rebalancing (Tier 3: target-weight multi-asset)
├── agent-trading-history  (Tier 3: signed append-only event log + verify)
├── agent-risk-exposure    (Tier 3: concentration dashboard)
├── agent-blocked-party    (Tier 3: blocklist settlement screener)
├── agent-hedging          (Tier 3: counter-exposure orders)
├── agent-block-execution  (Tier 3: TWAP × iceberg hybrid)
├── agent-batch-orders     (Tier 3: bulk submit / cancel CLI)
├── agent-compliance-screening (Tier 3: settlement rule-engine)
├── agent-liquidity-screening (Tier 3: depth + slippage analytics)
├── agent-target-allocation (Tier 3: absolute quote-value targets)
├── agent-liquidity-seeking (Tier 3: depth-adaptive execution)
├── agent-selective-disclosure (Tier 3: filtered + re-signed audit log)
├── agent-market-abuse     (Tier 3: spoof/layering self-audit)
├── agent-human-approval   (Tier 3: approval queue + sign-off)
├── agent-concentration-risk (Tier 3: cancel-on-breach enforcer)
├── agent-yield-rotation   (Tier 3: market carry ranking)
├── agent-treasury-mgmt    (Tier 3: policy + approval-routed rebalance)
├── agent-contractual-compliance (Tier 3: bilateral obligations)
├── agent-legal-compliance (Tier 3: jurisdiction rules)
├── agent-scam-screening   (Tier 3: categorized threat-feed)
├── agent-risk-management  (Tier 3: composite limits + enforce)
├── agent-inventory-risk   (Tier 3: soft/hard band hedge)
├── agent-smart-allocation (Tier 3: bucket portfolio)
├── agent-algo-order       (Tier 3: execution dispatcher)
├── agent-audit-attestation (Tier 3: signed chain-head checkpoints)
├── agent-audit-replay      (Tier 3: offline policy back-test on trading-history)
├── agent-audit-retention   (Tier 3: chained per-day/week log rotation)
└── agent-audit-anomaly     (Tier 3: post-hoc anomaly scan)
```

### gRPC Services

- **DAppProviderService** (ledger.proto) — Canton ledger operations via two-phase signing
- **OrderbookService** (orderbook.proto) — orders, markets, subscriptions
- **SettlementService** (settlement.proto) — bidirectional DVP lifecycle stream + RFQ
- **PricingService** (pricing.proto) — price feeds

### Configuration (per agent)

- `.env` — party keys, gRPC URLs, synchronizer ID
- `configuration.toml` — token registries, Canton Coin config
- `agent.toml` — agent-specific: markets, grid levels, RFQ params, polling

### Devnet

- Orderbook URL: configured in `.env` as `ORDERBOOK_GRPC_URL`
- Available market: **CC-USDC** (Canton Coin / USDC)
- Market IDs use format `BASE-QUOTE` (e.g. `CC-USDC`)

### What orderbook-agent-logic provides

- gRPC clients (orderbook, settlement, pricing, ledger)
- Settlement executor (full DVP workflow)
- Order submission + tracking + cancellation
- Auth (JWT), config loading, state persistence, logging
- Balance/liquidity tracking

### What each agent binary provides

- CLI (clap) with agent-specific commands
- `CloudSettlementBackend` (SettlementBackend trait impl)
- `DAppProviderClient` (ledger gRPC, two-phase signing)
- `PaymentQueue` + `AmuletCache` (amulet management)
- `AcsWorker` (background amulet refresh)
- Agent-specific business logic (DCA loop, TP/SL monitor, grid+RFQ)

### Build

```bash
# Local dev
cargo check
cargo build --release

# Individual agent
cargo build --release -p orderbook-cloud-agent
cargo build --release -p agent-spot-dca
cargo build --release -p agent-tpsl

# Docker (cloud-agent)
make build-arm    # Linux ARM64
make build-x86    # Linux x86_64
make build-mac    # macOS Apple Silicon
```

### Adding a New Agent

1. Create `crates/agent-<name>/` with `Cargo.toml` and `src/main.rs`
2. Add to workspace members in root `Cargo.toml`
3. Dependencies: `orderbook-agent-logic`, `orderbook-proto`, `tx-verifier`, `message-signing`
4. Copy backend.rs, ledger_client.rs, amulet_cache.rs, acs_worker.rs, payment_queue.rs from an existing agent (e.g. agent-spot-dca) as boilerplate
5. Implement agent-specific logic in main.rs
6. Build: `cargo build --release -p agent-<name>`

## Agents

### orderbook-cloud-agent (Orderbook Liquidity)

Grid orders + RFQ quoting + fill loops. The original agent.

```bash
cloud-agent agent                                    # Run (orders + settlement + RFQ)
cloud-agent buyer --market CC-USDC --amount 10       # Buy via RFQ fill loop
cloud-agent seller --market CC-USDC --amount 10      # Sell via RFQ fill loop
cloud-agent info balance                             # Show balances
```

### agent-spot-dca (Spot DCA)

Accumulate assets gradually at lower average cost on spot market.

```bash
agent-spot-dca run --market CC-USDC --amount 10 --interval 3600 --side buy
agent-spot-dca run --market CC-USDC --amount 10 --interval 3600 --side buy --price-offset-pct -0.5
agent-spot-dca run --market CC-USDC --amount 10 --interval 3600 --max-total 1000
agent-spot-dca status
```

### agent-tpsl (Automated TP/SL)

Take profit and stop loss with optional trailing stop.

```bash
agent-tpsl run --market CC-USDC --side long --quantity 10 --tp 1.5 --sl 1.2
agent-tpsl run --market CC-USDC --side long --quantity 10 --tp 1.5 --sl 1.2 --trailing-pct 2.0
agent-tpsl run --market CC-USDC --side short --quantity 10 --tp 1.0 --sl 1.4
agent-tpsl status
```

### agent-watchlist (Read-only market watcher)

Subscribes to external price feeds (`StreamPrices`) and Silvana internal orderbook depth
(`SubscribeOrderbook`) for a configurable set of markets and logs every update. No orders,
no settlement, no ledger writes — pure observability.

```bash
agent-watchlist run --markets CC-USDC,BTC-USD --depth 10
agent-watchlist run --markets CC-USDC --depth 20 --include-orderbook --include-trades
agent-watchlist run --markets CC-USDC --no-prices                # depth only
agent-watchlist run --markets CC-USDC --no-depth                 # external prices only
agent-watchlist snapshot --markets CC-USDC,BTC-USD --depth 5     # one-off snapshot
agent-watchlist markets                                          # list available markets
```

### agent-order-expiry (Order Expiry)

Periodically scans this party's active orders and cancels any whose age exceeds `--max-age-secs`.
Uses `OrderbookService.GetOrders` + `CancelOrder` — no two-phase signing needed (JWT only).

```bash
agent-order-expiry run --max-age-secs 3600 --check-interval 60
agent-order-expiry run --max-age-secs 600  --market CC-USDC --dry-run
agent-order-expiry status                              # list active orders with age
agent-order-expiry status --market CC-USDC
```

### agent-state-monitor (State Monitoring)

Read-only observer of the party's own state. Streams `SubscribeOrders` and `SubscribeSettlements`
in parallel and prints every event (created/filled/cancelled, proposal/settled/failed).

```bash
agent-state-monitor run                                # all markets, both streams
agent-state-monitor run --market CC-USDC
agent-state-monitor run --no-settlements               # orders only
agent-state-monitor snapshot --market CC-USDC          # one-off snapshot of orders + proposals
```

### agent-signature (off-chain Ed25519 signing)

CLI wrapper around `message-signing`. Reads the private key from `PARTY_AGENT_PRIVATE_KEY`
or `--private-key`; never sends it anywhere.

```bash
agent-signature public-key
agent-signature sign-raw --input "hello"
agent-signature sign-canonical --input-base64 <b64>          # ed25519-sha256-v1
agent-signature verify-raw --input "hello" --signature <b64> --public-key <b64url>
```

### agent-auth (JWT generation + inspection)

CLI wrapper around the JWT helpers in `orderbook-agent-logic::auth`.

```bash
agent-auth generate --role trader --ttl-secs 3600              # full JWT details
agent-auth generate --bare                                     # token only (for piping)
agent-auth decode --jwt <token>                                # pretty-print header + claims
agent-auth verify --jwt <token>                                # verify signature via embedded jwk
agent-auth user-id --jwt <token>                               # extract `sub` claim
```

### agent-dca-portfolio (multi-market DCA)

Same logic as `agent-spot-dca` but the order-placement loop is fanned out across every
market in `--markets`. Same `--amount` and `--interval` apply to each market.

```bash
agent-dca-portfolio run --markets CBTC-CC,CETH-CC --amount 0.001 --interval 3600 --side buy
agent-dca-portfolio run --markets CC-USDC --amount 10 --price-offset-pct -0.5 --max-total 1000
agent-dca-portfolio status
```

### agent-notifier (Notification + Hooks-and-Signals)

Forwards events from `SubscribeOrders`, `SubscribeSettlements`, `StreamPrices` to one or
more sinks (stdout / JSONL append file / HTTP webhook). Each event is a single JSON object
with `{ "kind": "...", "ts": "...", "payload": {...} }`.

```bash
agent-notifier run --orders --settlements --stdout
agent-notifier run --orders --settlements --webhook https://hooks.example.com/sink
agent-notifier run --price-markets CC-USDC,BTC-USD --log-file events.jsonl
agent-notifier run --orders --market CC-USDC --webhook https://… --stdout
```

### agent-cash-buffer (keep CC within a band)

Polls the agent's unlocked Canton Coin balance. When it exceeds `--max-cc`, the excess is
pushed to `--sink-party` via `TransferCc` (two-phase signed). Under-balance is logged as a
warning — agents can only PUSH, not PULL.

```bash
agent-cash-buffer run --min-cc 20 --max-cc 100 --sink-party <party-id> --check-interval 60
agent-cash-buffer run --min-cc 20 --max-cc 100 --sink-party <party-id> --dry-run
agent-cash-buffer status
```

### agent-killswitch (Tier 2 Risk — emergency stop)

Two modes. `panic` is one-shot (cancel everything now and exit). `run` monitors a health
condition on a schedule and trips when violated, exiting with code 2 so a supervisor can act.

```bash
agent-killswitch panic                                                  # cancel-all now
agent-killswitch panic --market CC-USDC
agent-killswitch run --max-open-orders 200 --check-interval 15
agent-killswitch run --max-failed-settlements 3 --max-open-orders 200
agent-killswitch run --max-open-orders 200 --dry-run                    # log only
```

### agent-twap (Tier 2 Trading — time-weighted execution)

Slice `--total` into `--slices` equal pieces, place one limit order per slice every
`duration_secs / slices` seconds. Each slice's price is mid ± `--price-offset-pct`, clamped
by optional `--limit-price` (worst acceptable).

```bash
agent-twap run --market CC-USDC --side buy --total 100 --slices 20 --duration-secs 3600
agent-twap run --market CC-USDC --side sell --total 50 --slices 10 --duration-secs 1800 \
              --price-offset-pct 0.1 --limit-price 0.95
agent-twap status
```

### agent-spot-grid (Tier 2 Trading — minimal grid)

Reads `[[markets]]` / `[[markets.bid_levels]]` / `[[markets.offer_levels]]` from `agent.toml`
(same format as `orderbook-cloud-agent`) and runs the shared `OrderManager` grid. No RFQ,
no fill loops — pure passive market making.

```bash
agent-spot-grid run
agent-spot-grid run --dry-run                # prepare orders without signing
agent-spot-grid status
```

### agent-mean-reversion (Tier 2 Trading — EMA snap-back)

Tracks an EMA over `--ema-window` poll samples. When mid diverges by more than
`--deviation-pct` from EMA, places one limit order at the EMA target. Won't stack: skips
when an open order already exists in the signal direction.

```bash
agent-mean-reversion run --market CC-USDC --quantity 1 --ema-window 20 --deviation-pct 1.5
agent-mean-reversion run --market CC-USDC --quantity 0.5 --poll-secs 10 --warmup-samples 30
agent-mean-reversion status
```

### agent-risk-alert (Tier 2 Risk — passive monitor)

Polls orderbook state and emits alerts when thresholds breach. Does NOT cancel orders — pair it with `agent-killswitch` if you also want enforcement.

```bash
agent-risk-alert run --max-open-orders 200 --max-failed-settlements 3 --stdout
agent-risk-alert run --max-open-notional 100000 --webhook https://hooks.example.com/sink
agent-risk-alert run --max-open-orders 100 --log-file alerts.jsonl --check-interval 60
agent-risk-alert check --max-open-orders 100        # one-off (non-zero exit on breach)
```

### agent-circuit-breaker (Tier 2 Risk — auto-pause)

Tracks a market's mid-price baseline over a rolling window. When deviation exceeds the threshold, cancels all this party's orders on that market and pauses.

```bash
agent-circuit-breaker run --market CC-USDC --max-deviation-pct 5.0 --window-secs 60
agent-circuit-breaker run --market CC-USDC --max-deviation-pct 2.5 --pause-secs 600 --dry-run
```

### agent-test-run (Tier 2 Tx Flow — pre-flight healthcheck)

Read-only RPC smoke test. Use it before launching ledger-writing agents to verify connectivity, JWT, and basic queries work. (Two-phase prepare/execute dry-runs are already covered by `--dry-run` on each ledger-writing agent.)

```bash
agent-test-run validate
agent-test-run validate --market CC-USDC          # adds GetPrice + GetOrderbookDepth checks
```

### agent-readiness-check (Tier 2 Tx Flow — pre-trade gate)

Verifies the agent is provisioned correctly: required balances, no stuck settlements, presence of TransferPreapproval contracts. Exits non-zero if any check fails — wire it into deployment scripts.

```bash
agent-readiness-check check --required Amulet=50 --required CC-USDC=100
agent-readiness-check check --max-failed-settlements 0 --max-pending-settlements 10 \
                            --require-preapproval --required Amulet=50
```

### agent-portfolio-health (Tier 2 Asset/Portfolio — exposure report)

Human-readable snapshot: balances per instrument, open orders aggregated by market and side, pending settlement notional, optional mid-price valuation of net open inventory.

```bash
agent-portfolio-health report
agent-portfolio-health report --markets CC-USDC,BTC-USD
```

### agent-orderbook-streaming (Tier 2 Data — market data fanout)

Subscribes to internal depth + external price stream for the given markets and forwards every update to any combination of stdout / append JSONL / HTTP webhook. Read-only, no party-specific state.

```bash
agent-orderbook-streaming run --markets CC-USDC --stdout
agent-orderbook-streaming run --markets CC-USDC,BTC-USD --log-file ticks.jsonl --depth 20
agent-orderbook-streaming run --markets CC-USDC --webhook https://hooks.example.com/feed --include-trades
```

### agent-oracle (Tier 2 Data — scheduled price publisher)

Polls `GetPrice` for every market on a schedule and emits one record per market per poll to sinks. Optional `--source` forces a specific upstream (binance_spot / bybit / coingecko).

```bash
agent-oracle run --markets CC-USDC,BTC-USD --poll-secs 30 --stdout
agent-oracle run --markets CC-USDC --source coingecko --webhook https://hooks.example.com/oracle
```

### agent-fair-value (Tier 2 Risk — reference price)

For each market polls every `--sources` entry separately and aggregates with `--method` (`median`, `mean`, or `trimmed-mean`). Emits one fair-value record per market per poll.

```bash
agent-fair-value run --markets CC-USDC --sources binance_spot,bybit,coingecko --stdout
agent-fair-value run --markets CC-USDC --sources binance_spot,bybit --method mean --webhook https://...
```

### agent-spread-capture (Tier 2 Trading — single-level MM)

Keeps one bid and one offer at `mid × (1 ∓ spread_bps/20000)`. Each refresh cycle cancels existing orders, recomputes prices, and re-quotes — skipping the bid side when net inventory > `+max-inventory`, the offer side when < `-max-inventory`.

```bash
agent-spread-capture run --market CC-USDC --spread-bps 25 --quantity 0.5 --max-inventory 10
agent-spread-capture run --market CC-USDC --spread-bps 50 --quantity 1.0 --max-inventory 5 --refresh-secs 15
```

### agent-signal-bot (Tier 2 Trading — JSONL signal executor)

Tails a JSONL file of trading signals and places one limit order per line. Persists its byte offset in `<signals>.cursor` so it survives restarts without replay.

Signal format:
```json
{"market":"CC-USDC","side":"buy","quantity":"0.5","price":"1.02","ref":"my-strategy-1"}
```

```bash
agent-signal-bot run --signals-file signals.jsonl
agent-signal-bot run --signals-file signals.jsonl --from-end --dry-run
```

### agent-inventory-mgmt (Tier 2 Trading — band-keeping rebalancer)

Polls the unlocked balance of `--instrument` and pushes orders to keep it inside `[target − tolerance, target + tolerance]`. Unlike `agent-cash-buffer` (which uses `TransferCc`), this one rebalances by placing orders on `--market`.

```bash
agent-inventory-mgmt run --market CC-USDC --instrument Amulet --target 100 --tolerance 20 --chunk-size 5
agent-inventory-mgmt run --market CC-USDC --instrument Amulet --target 100 --tolerance 5 \
                        --chunk-size 2 --check-interval 30 --price-offset-pct -0.1
agent-inventory-mgmt status
```

### agent-order-matching (Tier 2 Trading — trigger sniper)

Streams the orderbook depth for one market. When `best_offer <= --buy-trigger`, places a BID at that offer for `--quantity`. When `best_bid >= --sell-trigger`, places an OFFER at that bid. One open snipe per side at a time.

```bash
agent-order-matching run --market CC-USDC --buy-trigger 0.95 --quantity 5
agent-order-matching run --market CC-USDC --sell-trigger 1.10 --quantity 5
agent-order-matching run --market CC-USDC --buy-trigger 0.95 --sell-trigger 1.10 --quantity 5
```

### agent-witnesses (Tier 2 Tx Flow — events → command)

Subscribes to own orders + settlements and spawns a shell command per matched event-type. Event metadata is exported via `SILVANA_EVENT`, `SILVANA_EVENT_TS`, `SILVANA_PROPOSAL_ID`, `SILVANA_ORDER_ID`, `SILVANA_MARKET_ID`, `SILVANA_STATUS` env vars.

```bash
agent-witnesses run --on-settled '/scripts/notify-settled.sh'
agent-witnesses run --on-failed '/scripts/page-oncall.sh' --on-cancelled 'echo $SILVANA_PROPOSAL_ID >> cancels.log'
agent-witnesses run --on-order-filled '/scripts/record-fill.sh' --market CC-USDC
```

### agent-failure-recovery (Tier 2 Tx Flow — settlement watchdog)

Sweeps pending settlement proposals on a schedule. Surfaces PENDING proposals older than `--max-pending-age-secs` and any FAILED proposals. With `--cancel-related-orders` it also cancels any of the agent's own active orders linked to a stuck/failed proposal (via `OrderMatch.bid_order_id` / `offer_order_id`). Deep retry logic still lives in `settlement.rs` — this is a watchdog, not a replacement.

```bash
agent-failure-recovery run --max-pending-age-secs 3600 --check-interval 60
agent-failure-recovery run --max-pending-age-secs 1800 --cancel-related-orders
agent-failure-recovery run --cancel-related-orders --dry-run
agent-failure-recovery snapshot --max-pending-age-secs 600
```

### agent-infinite-grid (Tier 2 Trading — mid-following grid)

Regenerates a bid/offer ladder around the live mid every refresh, with no fixed price range. `--step-pct` is the spacing between adjacent levels and `--levels` the number per side. `--drift-threshold-pct` lets you avoid churning the book when the mid hasn't moved meaningfully since the previous build.

```bash
agent-infinite-grid run --market CC-USDC --step-pct 0.5 --levels 5 --quantity-per-level 0.1
agent-infinite-grid run --market CC-USDC --step-pct 0.25 --levels 10 --quantity-per-level 0.05 \
                       --refresh-secs 30 --drift-threshold-pct 0.1
agent-infinite-grid status
```

### agent-trend-analysis (Tier 3 Data — TA indicators)

Per market polls mid, keeps a rolling buffer, and publishes SMA / EMA / Bollinger / RSI / MACD on every tick to stdout / JSONL file / webhook. Read-only.

```bash
agent-trend-analysis run --markets CC-USDC --window 30 --stdout
agent-trend-analysis run --markets CC-USDC,BTC-USD --window 60 --rsi-period 14 \
                        --bollinger-k 2.0 --webhook https://hooks.example.com/ta
```

### agent-pairs-trading (Tier 3 Trading — dual-market divergence)

Tracks ratio `r = mid_A / mid_B`. When `r` diverges from `--target-ratio` by more than `--threshold-pct`, places one order per leg in opposite directions. Skips if a live order already exists in the intended direction on either leg. Minimal stat-arb skeleton — bring your own rolling mean/variance model for production use.

```bash
agent-pairs-trading run --market-a CC-USDC --market-b CC-USDT \
    --target-ratio 1.0 --threshold-pct 1.0 --quantity-a 10 --quantity-b 10
```

### agent-iceberg-execution (Tier 3 Execution — hidden parent)

Splits `--total` into successive `--visible` chunks. Each chunk is placed only after the previous chunk leaves the active book (filled or cancelled). At any moment the on-book footprint of this parent is ≤ `--visible`.

```bash
agent-iceberg-execution run --market CC-USDC --side buy --total 100 --visible 1 --price 1.02
agent-iceberg-execution run --market CC-USDC --side sell --total 50 --visible 2.5 --price 1.05 \
                            --max-runtime-secs 7200
```

### agent-volatility-screening (Tier 3 Risk — realized vol)

Maintains a rolling window of log returns over the last `--window` ticks and publishes the sample std-dev annualized to `realized_vol_annualized = std × sqrt(periods_per_year)`.

```bash
agent-volatility-screening run --markets CC-USDC --window 100 --stdout
agent-volatility-screening run --markets CC-USDC,BTC-USD --window 200 --poll-secs 10 \
                              --log-file vol.jsonl
```

### agent-pre-trade-check (Tier 3 Compliance — rule validator)

Pure offline rule engine. No network, no JWT — just validate one order (or a stream of orders from stdin) against a TOML rules file. Exits 0 on accept, 2 on reject.

```bash
agent-pre-trade-check check --market CC-USDC --side buy --quantity 10 --price 1.02 --rules rules.toml
cat orders.jsonl | agent-pre-trade-check check-stdin --rules rules.toml --echo > accepted.jsonl
```

Rules file (`rules.toml`):
```toml
max_notional_per_order = "5000"
max_quantity_per_order = "100"
min_price = "0.5"
max_price = "5.0"
blocked_markets = ["XXX-YYY"]
allowed_markets = ["CC-USDC", "BTC-USD"]
allowed_sides   = ["buy", "sell"]
```

### agent-pnl-screening (Tier 3 Risk — P&L tracker)

Streams settlement events and maintains per-market position, weighted-average cost basis, realized P&L, and live unrealized P&L. Snapshots emitted to stdout / JSONL file every `--snapshot-secs`.

```bash
agent-pnl-screening run --stdout
agent-pnl-screening run --market CC-USDC --log-file pnl.jsonl --snapshot-secs 60
```

### agent-trend-following (Tier 3 Trading — EMA crossover)

Two EMAs (`--fast`, `--slow`). Fast crosses above slow → BID entry; below → OFFER. Won't stack: skips when an open order in the signal direction already exists. `--warmup-samples` lets the EMAs stabilize before any signal is emitted.

```bash
agent-trend-following run --market CC-USDC --fast 9 --slow 21 --quantity 1
agent-trend-following run --market CC-USDC --fast 5 --slow 50 --quantity 0.5 --poll-secs 5 --warmup-samples 60
```

### agent-portfolio-rebalancing (Tier 3 Asset — target-weight rebalance)

Maintains target weights across instruments. Each cycle values your portfolio in quote currency via live mids, compares each instrument's current weight against target, and places a single rebalance order (BID under-weight, OFFER over-weight) on the configured market when the deviation exceeds `--threshold-pct`. `--rebalance-fraction` controls how aggressively each cycle closes the gap.

```bash
agent-portfolio-rebalancing run \
    --target Amulet@CC-USDC=0.4 \
    --target CBTC@CBTC-CC=0.6 \
    --threshold-pct 2.0 --rebalance-fraction 0.5

agent-portfolio-rebalancing status
```

### agent-trading-history (Tier 3 Auditing — signed event log)

Subscribes to own orders + settlements. Each event is written to a JSONL file as a signed, hash-chained record: every line carries `prev_hash` of the previous line plus an Ed25519 signature over `(seq, ts, prev_hash, kind, sha256(payload))`. Editing any line breaks the chain from that point on.

```bash
agent-trading-history run --history-file history.jsonl
agent-trading-history run --history-file history.jsonl --market CC-USDC --no-orders
agent-trading-history verify --history-file history.jsonl
```

### agent-risk-exposure (Tier 3 Risk — concentration dashboard)

Periodic snapshot of portfolio value (balances × live mids), open notional per market, pending settlement notional, and concentration share per instrument. Sets `concentration_warn` when any single instrument exceeds `--concentration-warn-pct`. Read-only.

```bash
agent-risk-exposure run --markets CC-USDC,BTC-USD --snapshot-secs 60 --stdout
agent-risk-exposure run --markets CC-USDC --log-file exposure.jsonl --concentration-warn-pct 70
agent-risk-exposure snapshot --markets CC-USDC,BTC-USD
```

### agent-blocked-party (Tier 3 Compliance — blocklist screener)

Streams settlements; flags any proposal whose buyer or seller appears in a plain-text blocklist file. Reloads the blocklist every `--reload-secs` so an operator can add entries without restarting.

```bash
agent-blocked-party run --blocklist blocked.txt --stdout
agent-blocked-party run --blocklist blocked.txt --webhook https://hooks.example.com/compliance --market CC-USDC
agent-blocked-party check --blocklist blocked.txt
```

### agent-hedging (Tier 3 Trading — counter-exposure)

Pushes the unlocked balance of `--exposure-instrument` back toward `--target-balance`. On every cycle: balance > target+tolerance → OFFER on hedge market; balance < target−tolerance → BID. Sized by `--hedge-fraction` of the deviation. Sibling of `agent-inventory-mgmt` but oriented around neutralizing unwanted exposure rather than maintaining a trading book.

```bash
agent-hedging run \
    --exposure-instrument Amulet --hedge-market CC-USDC \
    --target-balance 50 --tolerance 5 --hedge-fraction 0.5
```

### agent-block-execution (Tier 3 Tx Flow — TWAP × Iceberg)

Walks a large parent order across the book. Parent is split into `--time-slices` time slices; within each slice the visible quantity on the book is kept ≤ `--visible`. Unfilled remainder of a slice rolls into the next slice.

```bash
agent-block-execution run --market CC-USDC --side buy --total 100 --price 1.02 \
    --time-slices 10 --duration-secs 3600 --visible 2
```

### agent-batch-orders (Tier 3 Asset — bulk CLI)

One-shot commands for bulk operations.

```bash
# Submit a JSONL file of orders
agent-batch-orders submit-batch --file orders.jsonl
agent-batch-orders submit-batch --file orders.jsonl --abort-on-error

# Cancel filtered subsets
agent-batch-orders cancel-batch --market CC-USDC --side buy
agent-batch-orders cancel-batch --market CC-USDC
agent-batch-orders cancel-all

# Run settlement worker if your batch caused fills
agent-batch-orders settle
```

Each line in `orders.jsonl` is `{"market":"...","side":"buy|sell","quantity":"...","price":"...","ref":"..."}`. Blank lines and `#`-comments are ignored.

### agent-compliance-screening (Tier 3 Compliance — settlement rule-engine)

Generalization of `agent-blocked-party`. Applies a TOML policy (blocked pairs, per-party rolling daily notional caps, allowed-counterparty whitelist, blocked markets) to live settlement events. Emits `compliance.reject` or `compliance.accept` per match.

```bash
agent-compliance-screening run --policy policy.toml --stdout
agent-compliance-screening run --policy policy.toml --webhook https://hooks.example.com/compliance --emit-accepts
agent-compliance-screening check --policy policy.toml
```

### agent-liquidity-screening (Tier 3 Risk — depth + slippage)

Polls `GetOrderbookDepth` per market and publishes spread / spread_bps, bid+offer depth totals, and a slippage probe: for a configurable `--probe-qty`, walks the book to compute VWAP-vs-mid in basis points.

```bash
agent-liquidity-screening run --markets CC-USDC,BTC-USD --probe-qty 10 --stdout
agent-liquidity-screening run --markets CC-USDC --probe-qty 50 --depth 30 --log-file liq.jsonl
agent-liquidity-screening probe --market CC-USDC --probe-qty 25
```

### agent-target-allocation (Tier 3 Asset — absolute quote-value targets)

Sibling of `agent-portfolio-rebalancing` parameterized by absolute quote-currency value per instrument. Example: keep 10000 USDC of Amulet and 20000 CC of CBTC at any time, regardless of the rest of the portfolio.

```bash
agent-target-allocation run \
    --target Amulet@CC-USDC=10000 \
    --target CBTC@CBTC-CC=20000 \
    --threshold-quote 100 --rebalance-fraction 0.5
agent-target-allocation status
```

### agent-liquidity-seeking (Tier 3 Trading — adaptive execution)

Execute `--total` while keeping VWAP-vs-mid slippage of every child order under `--max-slippage-bps`. Each cycle walks the relevant side of the depth, sizes the child to the maximum quantity that respects the slippage budget (capped by `--max-chunk`), places it, waits for it to clear, repeats.

```bash
agent-liquidity-seeking run --market CC-USDC --side buy --total 100 \
    --max-slippage-bps 25 --max-chunk 5 --depth 20
agent-liquidity-seeking run --market CC-USDC --side sell --total 50 \
    --max-slippage-bps 50 --max-chunk 10 --max-runtime-secs 1800
```

### agent-selective-disclosure (Tier 3 Auditing — filtered & re-signed log)

Post-processor over an `agent-trading-history` JSONL file. Filters by event kind / market / redact-fields and writes a fresh hash-chained signed log of only the records you intend to share. Recipients can verify it with either `agent-selective-disclosure verify` or `agent-trading-history verify` — both share the same record schema.

```bash
agent-selective-disclosure filter --history history.jsonl --output disclosure.jsonl \
    --kinds settlement.settled,order.filled --markets CC-USDC \
    --redact-fields buyer,seller
agent-selective-disclosure verify --output disclosure.jsonl
```

### agent-market-abuse (Tier 3 Compliance — spoof/layering self-audit)

Streams the agent's own order events. Flags **spoofing** (a burst of fast cancels: `--spoof-burst` cancels within `--spoof-burst-window-secs` where each cancel landed ≤ `--spoof-window-secs` after the create) and **layering** (`--layer-min-orders` open same-side orders on one market clustered within `--layer-price-band-pct`). Visibility is limited to own flow by JWT scope — this is a self-audit / bug-catcher.

```bash
agent-market-abuse run --spoof-burst 10 --layer-min-orders 5 --layer-price-band-pct 1.0 --stdout
agent-market-abuse run --market CC-USDC --webhook https://hooks.example.com/abuse --log-file abuse.jsonl
```

### agent-human-approval (Tier 3 Compliance — approval queue)

File-backed queue (JSONL). Upstream agents (or scripts) append orders via `enqueue --file`; an operator runs `list` / `approve <id>` / `reject <id>` / `purge`. Approved orders are signed and submitted via `SubmitOrder`. The `settle` subcommand runs a background settlement worker.

```bash
agent-human-approval enqueue --file orders.jsonl
agent-human-approval list --status pending
agent-human-approval approve --id ha-... --by alice --reason "ok per policy"
agent-human-approval reject  --id ha-... --by bob   --reason "exceeds risk budget"
agent-human-approval purge
agent-human-approval settle
```

### agent-concentration-risk (Tier 3 Risk — cancel-on-breach enforcer)

Enforcement variant of `agent-risk-exposure`. When an instrument's share-of-portfolio exceeds `--max-share-pct`, cancels the agent's BIDs on that market (no further accumulation). Symmetrically, when it drops below `--min-share-pct`, cancels OFFERs (no further depletion). `--dry-run` only logs.

```bash
agent-concentration-risk run --markets CC-USDC,BTC-USD --max-share-pct 60
agent-concentration-risk run --markets CC-USDC --max-share-pct 50 --min-share-pct 10 --dry-run
agent-concentration-risk snapshot --markets CC-USDC,BTC-USD --max-share-pct 50
```

### agent-yield-rotation (Tier 3 Asset — carry ranking)

`score = w_change × change_24h_pct + w_volume × log10(volume_24h) − w_spread × spread_pct`. Emits the live ranking per cycle and a `yield.rotation_signal` whenever the top-ranked market changes. Read-only — pair with `agent-batch-orders` / a script to actually move capital.

```bash
agent-yield-rotation run --markets CC-USDC,BTC-USD,CETH-CC --stdout
agent-yield-rotation snapshot --markets CC-USDC,BTC-USD
```

### agent-treasury-mgmt (Tier 3 Asset — policy + approval routing)

Same target structure as `agent-target-allocation` (per-instrument absolute quote-value targets), with a treasury policy on top: per-trade ceiling, rolling 24h cap, and an approval threshold above which the trade goes into an `agent-human-approval` queue file instead of straight to the book.

```bash
agent-treasury-mgmt run \
    --target Amulet@CC-USDC=10000 \
    --target CBTC@CBTC-CC=20000 \
    --approval-threshold-quote 1000 \
    --max-trade-quote 5000 \
    --max-daily-trade-quote 25000 \
    --approval-queue approval-queue.jsonl
```

### agent-audit-attestation (Tier 3 Auditing — checkpoint publisher)

Reads the current head of an `agent-trading-history` JSONL file (last record's `seq`, `ts`, line-hash), wraps it in a signed `{ts, head_seq, head_hash, party, signature}` checkpoint, and emits to any combination of stdout / append JSONL file / HTTP webhook. Auditors can later use any single checkpoint to prove that earlier records existed at that time — without needing the full log. `snapshot` is one-shot; `run --interval-secs N` publishes on a schedule until SIGINT.

```bash
agent-audit-attestation snapshot --history history.jsonl --stdout --party party::alice
agent-audit-attestation run --history history.jsonl --interval-secs 300 \
                             --log-file checkpoints.jsonl \
                             --webhook https://hooks.example.com/attest
```

### agent-audit-replay (Tier 3 Auditing — offline policy back-test)

Reads a signed `agent-trading-history` JSONL and replays every `order.created` / `settlement.settled` record through a TOML rule set (same shape as `agent-pre-trade-check` + `agent-compliance-screening` per-market caps). Emits per-event verdicts + a summary with hits-by-rule. Answer "how many trades would have been blocked if this new policy had been active since date X?" before rolling it out live.

```bash
agent-audit-replay replay --history history.jsonl --rules rules.toml
agent-audit-replay replay --history history.jsonl --rules rules.toml --emit-accepts --output verdicts.jsonl
agent-audit-replay check --rules rules.toml
```

### agent-audit-retention (Tier 3 Auditing — chained log rotation)

Splits a large `agent-trading-history` JSONL into per-day (or per-week with `--weekly`) slice files. Each slice starts with a signed header that embeds the `prev_slice_hash` of the previous slice — the audit chain remains provable across rotations. With `--retention-days N`, records older than the window are dropped; the surviving slice headers still chain through. `verify` walks the slice directory in chronological order and checks the header chain.

```bash
agent-audit-retention rotate --history history.jsonl --out-dir slices/
agent-audit-retention rotate --history history.jsonl --out-dir slices/ --weekly --retention-days 90
agent-audit-retention verify --dir slices/
```

### agent-audit-anomaly (Tier 3 Auditing — post-hoc statistical scan)

Offline scan over any `agent-trading-history` JSONL. Applies heuristics that catch patterns worth reviewing after the fact — `stuck_settlement` (fill without a matching settled/failed inside the window), `rapid_cancel` (create+cancel within ms → spoofing indicator), `layer_cluster` (many same-side orders on one market inside a tight price band), `fill_before_cancel_burst` (cancel burst right after a fill → churn indicator). Distinct from live `agent-market-abuse`: works on any historical log, no stream connection.

```bash
agent-audit-anomaly --history history.jsonl
agent-audit-anomaly --history history.jsonl --layer-threshold 8 --layer-band-pct 0.5 --output anomalies.jsonl
agent-audit-anomaly --history history.jsonl --settlement-window-secs 1800 --burst-count 10
```

## Agent Roadmap

All agents reuse `orderbook-agent-logic` (settlement, orders, auth, config, liquidity, state) + `tx-verifier` + `message-signing`.
Complexity is based on how much new logic is needed on top of the shared core.

### Tier 1 — Minimal new logic (1-2 days)

Agents that are thin wrappers around existing core functionality. Mostly config + a simple loop.


| Agent                     | Category         | What it does                                                          | What to build on top of core                                      |
| ------------------------- | ---------------- | --------------------------------------------------------------------- | ----------------------------------------------------------------- |
| **Spot DCA** ✅            | Trading          | Accumulate assets gradually at lower average cost on spot             | Timer + config (amount, interval, instrument). ~20 lines of logic |
| **Dollar-Cost Averaging** ✅ | Asset/Portfolio | Build positions smoothly with scheduled entries over time             | Same as Spot DCA but portfolio-oriented config                    |
| **Order Expiry** ✅        | Tx Flow          | Remove stale quotes and orders when timing conditions end             | Order subscription (exists) + TTL timer + cancel call             |
| **Watchlist** ✅           | Data & Analytics | Track assets of your interest                                         | Read-only subscriptions, no orders or settlement                  |
| **State Monitoring** ✅    | Data & Analytics | Watch fills, open orders, rejects, settlement proposals               | Subscribe to order/settlement streams, log/display state          |
| **Notification** ✅        | Data & Analytics | Push alerts in real-time for fills, proposals, status, data feeds     | Subscriptions + webhook/HTTP call on trigger                      |
| **Cash Buffer** ✅         | Asset/Portfolio  | Maintain idle liquidity for settlement, fees, and fast reactions      | GetBalances + TransferCc (both exist in ledger client)            |
| **Automated TP/SL** ✅     | Trading          | Set Take Profit and Stop Loss                                         | Price subscription + conditional cancel/place orders              |
| **Hooks and Signals** ✅   | Data & Analytics | Notify on events you subscribed for: price, fills, news               | Event subscription + configurable output sink                     |
| **Auth** ✅                | Tx Flow          | Auto Login, Smart login, 2FA, and other auth functions                | Wraps existing JWT/signing                                        |
| **Signature** ✅           | Tx Flow          | Perform sensitive cryptographic signing in a secure off-chain runtime | Wraps existing Ed25519 signing module                             |


### Tier 2 — Moderate logic (3-5 days)

Agents that need some new business logic but heavily reuse core infrastructure.


| Agent                      | Category         | What it does                                                              | What to build                                                 |
| -------------------------- | ---------------- | ------------------------------------------------------------------------- | ------------------------------------------------------------- |
| **Spot Grid** ✅            | Trading          | Automate buy-low, sell-high across every market swing                     | Subset of orderbook-cloud-agent — strip RFQ, fill loops       |
| **Futures Grid**           | Trading          | Run grid strategies using futures markets                                 | Grid logic adapted for futures contracts                      |
| **Infinite Grid** ✅        | Trading          | Trade grid strategies without fixed price ranges                          | Dynamic grid that expands/contracts with price                |
| **Margin Grid**            | Trading          | Boost grid returns using leverage                                         | Grid + margin/leverage management                             |
| **Mean Reversion** ✅       | Trading          | Trade the snapback when price pushes too far from fair value              | Price history buffer + EMA/SMA + order placement              |
| **Spread Capture** ✅       | Trading          | Consistently harvest bid-ask spread without directional risk              | Two-sided quoting with inventory tracking                     |
| **Signal Bot** ✅           | Trading          | Trade based on trading signals                                            | Signal input + order execution on trigger                     |
| **Futures DCA**            | Trading          | Scale futures positions with automated entries                            | DCA logic adapted for futures, position sizing                |
| **Order Matching** ✅       | Trading          | Match automatically bids and asks                                         | Order book scanning + matching logic                          |
| **Readiness Check** ✅      | Tx Flow          | Check whether both sides have reserved required assets before settlement  | Orchestrate balance/amulet/preapproval queries                |
| **Failure Recovery** ✅     | Tx Flow          | Detect failed or stale flows, retry/cancel/rollback transactions          | Settlement state inspection + retry/cancel logic              |
| **Test Run** ✅             | Tx Flow          | Pre-run a transaction to check for inconsistencies, possible failure risk | Dry-run PrepareTransaction without ExecuteTransaction         |
| **Witnesses** ✅            | Tx Flow          | Listen to events that launch transactions                                 | Event stream subscription + transaction tracking              |
| **Orderbook Streaming** ✅  | Data & Analytics | Stream orderbook depth, trades, prices to strategy engine in real time    | Subscription wrappers + output format (WebSocket/file/stdout) |
| **Oracle** ✅               | Data & Analytics | Fetch verified data from off-chain feeds                                  | External API integration + price feed publishing              |
| **Portfolio Health Check** ✅ | Asset/Portfolio | Breakdown of P&L, positions, profitability analysis                       | Balance aggregation + multi-market reporting                  |
| **Risk Alert** ✅           | Risk             | Signal if any risk comes up                                               | Balance polling + configurable thresholds + alert sink        |
| **Fair Value Screening** ✅ | Risk             | Calculate internal reference price to guide quoting and execution         | Multi-source price aggregation + fair value model             |
| **Killswitch** ✅           | Risk             | Stop quoting or trading instantly when risk or connectivity breaks        | Health monitoring + emergency cancel-all                      |
| **Circuit Breaker** ✅      | Risk             | Temporarily halt trading during abnormal price moves or stress            | Price deviation detection + trading pause/resume              |
| **Inventory Management** ✅ | Trading          | Keep market-making inventories within safe ranges                         | Position tracking + rebalancing orders                        |
| **TWAP Execution** ✅       | Trading          | Spread a large trade into smaller timed slices to reduce impact           | Time-sliced order scheduler + fill tracking                   |
| **Copy Trading**           | Trading          | Copy trading leaders' positions                                           | Position subscription + mirror order placement                |


### Tier 3 — Substantial new logic (1-2 weeks)

Agents requiring significant new business logic, analytics, or external integrations.


| Agent                                      | Category         | What it does                                                                      | Why it's harder                                                       |
| ------------------------------------------ | ---------------- | --------------------------------------------------------------------------------- | --------------------------------------------------------------------- |
| **Trend-Following / Spot Trend Following** ✅ | Trading          | Ride strong market momentum with disciplined entry and exit                       | Price analytics, indicators (EMA crossover, RSI), signal generation   |
| **Futures Trend Following**                | Trading          | Follow big narratives and market trends for futures                               | Same as above + futures-specific position management                  |
| **Scalping**                               | Trading          | Capture rapid micro-moves with high-speed, event-driven execution                 | Low-latency path, tick-level data, fast cancel/replace                |
| **News-Driven Trading**                    | Trading          | Trade based on recent news, do it promptly                                        | News feed integration, NLP/sentiment analysis, fast execution         |
| **Pairs Trading** ✅                        | Trading          | Trade two related assets against each other when spread diverges                  | Correlation tracking, spread calculation, dual-leg orders             |
| **Cross-chain Arbitrage**                  | Trading          | Move on pricing gaps fast across venues and market environments                   | Multi-venue connectivity, atomic execution, latency optimization      |
| **Inter-Exchange Arbitrage**               | Trading          | Exploit price differences across exchanges                                        | External exchange APIs, cross-venue order routing                     |
| **Cross-chain Opportunity**                | Trading          | Find opportunities between Canton-settled instruments and other chains            | Multi-chain connectivity, bridge integration                          |
| **Cross-Venue Hedging**                    | Trading          | Offset risk opened on Silvana Book using correlated positions on external markets | External venue APIs + hedge ratio calculation                         |
| **Hedging** ✅                              | Trading          | Place offsetting trades to reduce unwanted market exposure                        | Position analysis + hedge instrument selection + execution            |
| **Liquidity Seeking** ✅                    | Trading          | Choose best moment and order style to complete a trade with lower slippage        | Market microstructure analysis, adaptive order types                  |
| **Algo Order** ✅                           | Trading          | Automated trading order executing according to predefined parameters              | Generic algo framework with pluggable strategies                      |
| **Futures TWAP**                           | Trading          | Execute large orders gradually over time on futures                               | TWAP + futures-specific logic (margin, funding)                       |
| **Futures VP**                             | Trading          | Algorithmic futures order that executes in proportion to market volume            | Volume profiling + participation rate targeting                       |
| **Position Snowball**                      | Trading          | Earn high yield if asset price stays within a specified range until expiration    | Range detection + position compounding logic                          |
| **Block Execution** ✅                      | Tx Flow          | Handle large private trades with minimal signaling and controlled settlement      | Large trade chunking, dark pool logic, controlled information leakage |
| **Liquidity Provider**                     | Tx Flow          | Deploy liquidity discreetly and capture flow without revealing strategy           | Advanced RFQ logic, stealth quoting, flow toxicity analysis           |
| **Strategy Orchestrator**                  | Tx Flow          | Combine multiple specialist agents into one controlled trading workflow           | Multi-agent coordination, shared state, workflow engine               |
| **Iceberg Execution** ✅                    | Data & Analytics | Only small visible order fragments exposed while working a larger hidden position | Chunk splitting, fill tracking, adaptive reveal                       |
| **AI Agent**                               | Data & Analytics | Let intelligent agents monitor markets and execute on your behalf                 | ML model integration, decision framework                              |
| **AI Decision**                            | Data & Analytics | Run model-driven decision logic on market and contextual inputs                   | ML inference pipeline, feature engineering                            |
| **Trend Analysis** ✅                       | Data & Analytics | Get in-depth market trends with complex and compound indicators                   | Technical analysis library, multi-timeframe analysis                  |
| **Performance Dashboard**                  | Data & Analytics | Analyze whether returns came from spread capture, direction, or arbitrage         | P&L attribution, strategy tagging, reporting engine                   |
| **Chat-to-Strategy**                       | Data & Analytics | Create trading strategies directly with AI                                        | LLM integration, strategy DSL, code generation                        |
| **Portfolio Rebalancing** ✅                | Asset/Portfolio  | Keep allocations on target with automated, risk-aware portfolio moves             | Multiple markets, weight calculation, batch order coordination        |
| **Target Allocation** ✅                    | Asset/Portfolio  | Move capital toward pre-set exposures such as cash, BTC, or strategy buckets      | Multi-asset allocation logic, rebalancing triggers                    |
| **Smart Allocation** ✅                     | Asset/Portfolio  | Build and manage rule-based baskets for structured or theme-based markets         | Basket composition, rule engine, dynamic rebalancing                  |
| **Treasury Management** ✅                  | Asset/Portfolio  | Execute company treasury trades under predefined rules for allocation, liquidity  | Policy engine, multi-account, approval workflows                      |
| **Lending and Financing**                  | Asset/Portfolio  | Automate flows for lending or structured-product markets                          | DeFi protocol integration, rate optimization                          |
| **Yield Rotation** ✅                       | Asset/Portfolio  | Shift capital between instruments when risk-adjusted carry or returns improve     | Multi-instrument yield analysis, rotation triggers                    |
| **Batch Order Management** ✅               | Asset/Portfolio  | Manage big bulks of trading positions with one click                              | Batch order builder, position grouping, bulk cancel/modify            |
| **Pre-Trade Check** ✅                      | Compliance       | Check orders for compliance to pre-defined rules, block violations                | Rule engine, policy configuration, reject/approve flow                |
| **Compliance Screening** ✅                 | Compliance       | Check counterparties and flows against eligibility and restriction rules          | External data integration, rule matching                              |
| **Contractual Compliance** ✅               | Compliance       | Verify party compliance with contractual obligations                              | Contract rule sets, obligation tracking                               |
| **Legal Compliance** ✅                     | Compliance       | Verify party compliance with jurisdiction legal obligations                       | Legal rule sets, jurisdiction mapping                                 |
| **Human Approval** ✅                       | Compliance       | Manual sign-off step for larger or exceptional trades                             | UI/notification integration, approval queue, timeout handling         |
| **Scam Screening** ✅                       | Compliance       | Check parties and assets for potential scam activity                              | External threat feeds, anomaly detection                              |
| **Blocked Party Detection** ✅              | Compliance       | Track and flag blocked parties                                                    | Blocklist management, real-time party screening                       |
| **Market Abuse Screening** ✅               | Compliance       | Look for spoofing, layering, or abnormal trading patterns                         | Pattern detection, statistical analysis, alert generation             |
| **Volatility Screening** ✅                 | Risk             | Track market price fluctuations to adjust strategies and evaluate risks           | Volatility models (historical, implied), risk metrics                 |
| **Liquidity Screening** ✅                  | Risk             | Measure available depth, fill quality, slippage risk before orders sent           | Orderbook depth analysis, slippage estimation                         |
| **PnL Screening** ✅                        | Risk             | Track realized and unrealized performance in real time across strategies          | Position tracking, cost basis, multi-market aggregation               |
| **Risk Exposure Dashboard** ✅              | Risk             | Summarize positions, concentrations, and risk across wallets and markets          | Cross-agent aggregation, risk metrics, reporting                      |
| **Risk Management** ✅                      | Risk             | Check every strategy action against user-defined limits and risk policies         | Rule engine, position limits, exposure calculation                    |
| **Concentration Risk Prevention** ✅        | Risk             | Stop strategies from becoming too concentrated in one asset or market             | Cross-agent position awareness, global limit checks                   |
| **Inventory Risk Prevention** ✅            | Risk             | Cut or hedge positions when a market maker's inventory becomes too directional    | Inventory analysis + automated hedging triggers                       |
| **Trading History** ✅                      | Auditing         | Build a provable history of decisions, trades, and settlement outcomes            | Event sourcing, immutable log, query interface                        |
| **Selective Disclosure** ✅                 | Auditing         | Share only the minimum provable facts needed for auditors or regulators           | Data filtering, access control, proof generation                      |
| **Audit Attestation** ✅                    | Auditing         | Publish signed checkpoints of the trading-history chain head                      | Chain-head sha256, Ed25519 signing, periodic publisher                |
| **Audit Replay** ✅                         | Auditing         | Back-test a rule policy against a signed trading-history log                      | JSONL streamer + TOML rule engine reused from pre-trade-check         |
| **Audit Retention** ✅                      | Auditing         | Split trading-history into signed, chained per-day/week slices with retention     | Bucket-by-day, signed slice header, prev_slice_hash cross-file chain  |
| **Audit Anomaly** ✅                        | Auditing         | Offline scan for stuck settlements, rapid cancels, layer clusters, cancel bursts  | Heuristic pattern matcher over historical JSONL                       |
| **Simulation Agent**                       | Testing          | Test a strategy against live-like market conditions before capital is deployed    | Market simulation engine, virtual order matching                      |
| **Backtesting Agent**                      | Testing          | Evaluate a strategy on historical data before it is allowed to production         | Historical data replay, performance metrics                           |
| **Custom Bot**                             | Trading          | Build and run your own trading strategies                                         | Generic strategy framework, plugin system                             |


### Tier 4 — Completely different stack (weeks-months)


| Agent                               | Category   | What it does                                                              | Why it's a separate effort                             |
| ----------------------------------- | ---------- | ------------------------------------------------------------------------- | ------------------------------------------------------ |
| **Proving**                         | ZK Proving | Generate zero-knowledge proofs for orderbook operations                   | ZK circuit design, proof systems (Groth16/Plonk/STARK) |
| **Proof-of-Funds**                  | ZK Proving | Verify trader can support order size before acceptance                    | ZK balance proof without revealing exact amount        |
| **Proof-of-Trade-Activity**         | ZK Proving | Produce proofs about trading activity without revealing individual orders | ZK aggregation proofs, privacy-preserving analytics    |
| **Proof-of-Aggregate-Data**         | ZK Proving | Prove totals (avg price, volume) while keeping raw trades private         | ZK sum/average circuits, commitment schemes            |
| **Proof-of-Contractual-Compliance** | ZK Proving | Prove party complies with contractual obligations                         | ZK compliance circuits, contract encoding              |
| **Proof-of-Legal-Compliance**       | ZK Proving | Prove party complies with legal obligations of its jurisdiction           | ZK legal rule circuits, jurisdiction encoding          |
| **Proof-of-Risk-Compliance**        | ZK Proving | Prove party complies with risk policies                                   | ZK risk metric circuits                                |


