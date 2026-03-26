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
    └── agent-tpsl/              # Automated TP/SL agent
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
└── agent-tpsl             (take profit / stop loss)
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

## Agent Roadmap

All agents reuse `orderbook-agent-logic` (settlement, orders, auth, config, liquidity, state) + `tx-verifier` + `message-signing`.
Complexity is based on how much new logic is needed on top of the shared core.

### Tier 1 — Minimal new logic (1-2 days)

Agents that are thin wrappers around existing core functionality. Mostly config + a simple loop.


| Agent                     | Category         | What it does                                                          | What to build on top of core                                      |
| ------------------------- | ---------------- | --------------------------------------------------------------------- | ----------------------------------------------------------------- |
| **Spot DCA** ✅            | Trading          | Accumulate assets gradually at lower average cost on spot             | Timer + config (amount, interval, instrument). ~20 lines of logic |
| **Dollar-Cost Averaging** | Asset/Portfolio  | Build positions smoothly with scheduled entries over time             | Same as Spot DCA but portfolio-oriented config                    |
| **Order Expiry**          | Tx Flow          | Remove stale quotes and orders when timing conditions end             | Order subscription (exists) + TTL timer + cancel call             |
| **Watchlist**             | Data & Analytics | Track assets of your interest                                         | Read-only subscriptions, no orders or settlement                  |
| **State Monitoring**      | Data & Analytics | Watch fills, open orders, rejects, settlement proposals               | Subscribe to order/settlement streams, log/display state          |
| **Notification**          | Data & Analytics | Push alerts in real-time for fills, proposals, status, data feeds     | Subscriptions + webhook/HTTP call on trigger                      |
| **Cash Buffer**           | Asset/Portfolio  | Maintain idle liquidity for settlement, fees, and fast reactions      | GetBalances + TransferCc (both exist in ledger client)            |
| **Automated TP/SL** ✅     | Trading          | Set Take Profit and Stop Loss                                         | Price subscription + conditional cancel/place orders              |
| **Hooks and Signals**     | Data & Analytics | Notify on events you subscribed for: price, fills, news               | Event subscription + configurable output sink                     |
| **Auth**                  | Tx Flow          | Auto Login, Smart login, 2FA, and other auth functions                | Wraps existing JWT/signing                                        |
| **Signature**             | Tx Flow          | Perform sensitive cryptographic signing in a secure off-chain runtime | Wraps existing Ed25519 signing module                             |


### Tier 2 — Moderate logic (3-5 days)

Agents that need some new business logic but heavily reuse core infrastructure.


| Agent                      | Category         | What it does                                                              | What to build                                                 |
| -------------------------- | ---------------- | ------------------------------------------------------------------------- | ------------------------------------------------------------- |
| **Spot Grid**              | Trading          | Automate buy-low, sell-high across every market swing                     | Subset of orderbook-cloud-agent — strip RFQ, fill loops       |
| **Futures Grid**           | Trading          | Run grid strategies using futures markets                                 | Grid logic adapted for futures contracts                      |
| **Infinite Grid**          | Trading          | Trade grid strategies without fixed price ranges                          | Dynamic grid that expands/contracts with price                |
| **Margin Grid**            | Trading          | Boost grid returns using leverage                                         | Grid + margin/leverage management                             |
| **Mean Reversion**         | Trading          | Trade the snapback when price pushes too far from fair value              | Price history buffer + EMA/SMA + order placement              |
| **Spread Capture**         | Trading          | Consistently harvest bid-ask spread without directional risk              | Two-sided quoting with inventory tracking                     |
| **Signal Bot**             | Trading          | Trade based on trading signals                                            | Signal input + order execution on trigger                     |
| **Futures DCA**            | Trading          | Scale futures positions with automated entries                            | DCA logic adapted for futures, position sizing                |
| **Order Matching**         | Trading          | Match automatically bids and asks                                         | Order book scanning + matching logic                          |
| **Readiness Check**        | Tx Flow          | Check whether both sides have reserved required assets before settlement  | Orchestrate balance/amulet/preapproval queries                |
| **Failure Recovery**       | Tx Flow          | Detect failed or stale flows, retry/cancel/rollback transactions          | Settlement state inspection + retry/cancel logic              |
| **Test Run**               | Tx Flow          | Pre-run a transaction to check for inconsistencies, possible failure risk | Dry-run PrepareTransaction without ExecuteTransaction         |
| **Witnesses**              | Tx Flow          | Listen to events that launch transactions                                 | Event stream subscription + transaction tracking              |
| **Orderbook Streaming**    | Data & Analytics | Stream orderbook depth, trades, prices to strategy engine in real time    | Subscription wrappers + output format (WebSocket/file/stdout) |
| **Oracle**                 | Data & Analytics | Fetch verified data from off-chain feeds                                  | External API integration + price feed publishing              |
| **Portfolio Health Check** | Asset/Portfolio  | Breakdown of P&L, positions, profitability analysis                       | Balance aggregation + multi-market reporting                  |
| **Risk Alert**             | Risk             | Signal if any risk comes up                                               | Balance polling + configurable thresholds + alert sink        |
| **Fair Value Screening**   | Risk             | Calculate internal reference price to guide quoting and execution         | Multi-source price aggregation + fair value model             |
| **Killswitch**             | Risk             | Stop quoting or trading instantly when risk or connectivity breaks        | Health monitoring + emergency cancel-all                      |
| **Circuit Breaker**        | Risk             | Temporarily halt trading during abnormal price moves or stress            | Price deviation detection + trading pause/resume              |
| **Inventory Management**   | Trading          | Keep market-making inventories within safe ranges                         | Position tracking + rebalancing orders                        |
| **TWAP Execution**         | Trading          | Spread a large trade into smaller timed slices to reduce impact           | Time-sliced order scheduler + fill tracking                   |
| **Copy Trading**           | Trading          | Copy trading leaders' positions                                           | Position subscription + mirror order placement                |


### Tier 3 — Substantial new logic (1-2 weeks)

Agents requiring significant new business logic, analytics, or external integrations.


| Agent                                      | Category         | What it does                                                                      | Why it's harder                                                       |
| ------------------------------------------ | ---------------- | --------------------------------------------------------------------------------- | --------------------------------------------------------------------- |
| **Trend-Following / Spot Trend Following** | Trading          | Ride strong market momentum with disciplined entry and exit                       | Price analytics, indicators (EMA crossover, RSI), signal generation   |
| **Futures Trend Following**                | Trading          | Follow big narratives and market trends for futures                               | Same as above + futures-specific position management                  |
| **Scalping**                               | Trading          | Capture rapid micro-moves with high-speed, event-driven execution                 | Low-latency path, tick-level data, fast cancel/replace                |
| **News-Driven Trading**                    | Trading          | Trade based on recent news, do it promptly                                        | News feed integration, NLP/sentiment analysis, fast execution         |
| **Pairs Trading**                          | Trading          | Trade two related assets against each other when spread diverges                  | Correlation tracking, spread calculation, dual-leg orders             |
| **Cross-chain Arbitrage**                  | Trading          | Move on pricing gaps fast across venues and market environments                   | Multi-venue connectivity, atomic execution, latency optimization      |
| **Inter-Exchange Arbitrage**               | Trading          | Exploit price differences across exchanges                                        | External exchange APIs, cross-venue order routing                     |
| **Cross-chain Opportunity**                | Trading          | Find opportunities between Canton-settled instruments and other chains            | Multi-chain connectivity, bridge integration                          |
| **Cross-Venue Hedging**                    | Trading          | Offset risk opened on Silvana Book using correlated positions on external markets | External venue APIs + hedge ratio calculation                         |
| **Hedging**                                | Trading          | Place offsetting trades to reduce unwanted market exposure                        | Position analysis + hedge instrument selection + execution            |
| **Liquidity Seeking**                      | Trading          | Choose best moment and order style to complete a trade with lower slippage        | Market microstructure analysis, adaptive order types                  |
| **Algo Order**                             | Trading          | Automated trading order executing according to predefined parameters              | Generic algo framework with pluggable strategies                      |
| **Futures TWAP**                           | Trading          | Execute large orders gradually over time on futures                               | TWAP + futures-specific logic (margin, funding)                       |
| **Futures VP**                             | Trading          | Algorithmic futures order that executes in proportion to market volume            | Volume profiling + participation rate targeting                       |
| **Position Snowball**                      | Trading          | Earn high yield if asset price stays within a specified range until expiration    | Range detection + position compounding logic                          |
| **Block Execution**                        | Tx Flow          | Handle large private trades with minimal signaling and controlled settlement      | Large trade chunking, dark pool logic, controlled information leakage |
| **Liquidity Provider**                     | Tx Flow          | Deploy liquidity discreetly and capture flow without revealing strategy           | Advanced RFQ logic, stealth quoting, flow toxicity analysis           |
| **Strategy Orchestrator**                  | Tx Flow          | Combine multiple specialist agents into one controlled trading workflow           | Multi-agent coordination, shared state, workflow engine               |
| **Iceberg Execution**                      | Data & Analytics | Only small visible order fragments exposed while working a larger hidden position | Chunk splitting, fill tracking, adaptive reveal                       |
| **AI Agent**                               | Data & Analytics | Let intelligent agents monitor markets and execute on your behalf                 | ML model integration, decision framework                              |
| **AI Decision**                            | Data & Analytics | Run model-driven decision logic on market and contextual inputs                   | ML inference pipeline, feature engineering                            |
| **Trend Analysis**                         | Data & Analytics | Get in-depth market trends with complex and compound indicators                   | Technical analysis library, multi-timeframe analysis                  |
| **Performance Dashboard**                  | Data & Analytics | Analyze whether returns came from spread capture, direction, or arbitrage         | P&L attribution, strategy tagging, reporting engine                   |
| **Chat-to-Strategy**                       | Data & Analytics | Create trading strategies directly with AI                                        | LLM integration, strategy DSL, code generation                        |
| **Portfolio Rebalancing**                  | Asset/Portfolio  | Keep allocations on target with automated, risk-aware portfolio moves             | Multiple markets, weight calculation, batch order coordination        |
| **Target Allocation**                      | Asset/Portfolio  | Move capital toward pre-set exposures such as cash, BTC, or strategy buckets      | Multi-asset allocation logic, rebalancing triggers                    |
| **Smart Allocation**                       | Asset/Portfolio  | Build and manage rule-based baskets for structured or theme-based markets         | Basket composition, rule engine, dynamic rebalancing                  |
| **Treasury Management**                    | Asset/Portfolio  | Execute company treasury trades under predefined rules for allocation, liquidity  | Policy engine, multi-account, approval workflows                      |
| **Lending and Financing**                  | Asset/Portfolio  | Automate flows for lending or structured-product markets                          | DeFi protocol integration, rate optimization                          |
| **Yield Rotation**                         | Asset/Portfolio  | Shift capital between instruments when risk-adjusted carry or returns improve     | Multi-instrument yield analysis, rotation triggers                    |
| **Batch Order Management**                 | Asset/Portfolio  | Manage big bulks of trading positions with one click                              | Batch order builder, position grouping, bulk cancel/modify            |
| **Pre-Trade Check**                        | Compliance       | Check orders for compliance to pre-defined rules, block violations                | Rule engine, policy configuration, reject/approve flow                |
| **Compliance Screening**                   | Compliance       | Check counterparties and flows against eligibility and restriction rules          | External data integration, rule matching                              |
| **Contractual Compliance**                 | Compliance       | Verify party compliance with contractual obligations                              | Contract rule sets, obligation tracking                               |
| **Legal Compliance**                       | Compliance       | Verify party compliance with jurisdiction legal obligations                       | Legal rule sets, jurisdiction mapping                                 |
| **Human Approval**                         | Compliance       | Manual sign-off step for larger or exceptional trades                             | UI/notification integration, approval queue, timeout handling         |
| **Scam Screening**                         | Compliance       | Check parties and assets for potential scam activity                              | External threat feeds, anomaly detection                              |
| **Blocked Party Detection**                | Compliance       | Track and flag blocked parties                                                    | Blocklist management, real-time party screening                       |
| **Market Abuse Screening**                 | Compliance       | Look for spoofing, layering, or abnormal trading patterns                         | Pattern detection, statistical analysis, alert generation             |
| **Volatility Screening**                   | Risk             | Track market price fluctuations to adjust strategies and evaluate risks           | Volatility models (historical, implied), risk metrics                 |
| **Liquidity Screening**                    | Risk             | Measure available depth, fill quality, slippage risk before orders sent           | Orderbook depth analysis, slippage estimation                         |
| **PnL Screening**                          | Risk             | Track realized and unrealized performance in real time across strategies          | Position tracking, cost basis, multi-market aggregation               |
| **Risk Exposure Dashboard**                | Risk             | Summarize positions, concentrations, and risk across wallets and markets          | Cross-agent aggregation, risk metrics, reporting                      |
| **Risk Management**                        | Risk             | Check every strategy action against user-defined limits and risk policies         | Rule engine, position limits, exposure calculation                    |
| **Concentration Risk Prevention**          | Risk             | Stop strategies from becoming too concentrated in one asset or market             | Cross-agent position awareness, global limit checks                   |
| **Inventory Risk Prevention**              | Risk             | Cut or hedge positions when a market maker's inventory becomes too directional    | Inventory analysis + automated hedging triggers                       |
| **Trading History**                        | Auditing         | Build a provable history of decisions, trades, and settlement outcomes            | Event sourcing, immutable log, query interface                        |
| **Selective Disclosure**                   | Auditing         | Share only the minimum provable facts needed for auditors or regulators           | Data filtering, access control, proof generation                      |
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


