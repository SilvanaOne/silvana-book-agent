# Silvana Book Agent

Cloud agent for liquidity providers on the Silvana orderbook. Runs without direct Canton ledger access — all ledger operations are proxied through the Silvana gRPC service. Your Ed25519 private key never leaves the agent; transactions are signed locally using a two-phase protocol.

Two modes of providing liquidity:

- **Grid orders** — passive limit orders placed at configurable price levels around the mid price
- **RFQ (Request for Quote)** — real-time quote responses to incoming swap requests via bidirectional gRPC stream

## Prerequisites

- [Rust](https://rustup.rs/) (stable toolchain)
- Protocol Buffers compiler: `apt install protobuf-compiler` (Linux) or `brew install protobuf` (macOS)
- Silvana orderbook RPC endpoint URL (provided by Silvana)

## Build

```bash
cargo build --release -p orderbook-cloud-agent
```

The binary is at `target/release/cloud-agent`.

## Onboarding

Self-service onboarding generates an Ed25519 keypair, registers with the orderbook server, signs the Canton topology transaction, and completes ledger setup (preapproval, user service, subscription) — all in one command:

```bash
cloud-agent onboard --rpc https://rpc.example.com
```

This populates your `.env` file with all required configuration. The command is idempotent and can be re-run safely.

### Options

| Flag | Description |
|------|-------------|
| `--rpc <URL>` | Orderbook gRPC endpoint (required) |
| `--invite-code <CODE>` | Invite code for waiting list registration |
| `--agent-name <NAME>` | Display name for the agent |
| `--email <EMAIL>` | Contact email |
| `--party <ID>` | Skip waiting list (requires `--private-key`) |
| `--private-key <B58>` | Base58-encoded Ed25519 private key |
| `--env-file <PATH>` | Path to .env file (default: `.env`) |
| `--poll-interval <SECS>` | Polling interval during onboarding (default: 10) |

## Configuration

The agent reads from three files:

### `.env` — Environment Variables

Auto-populated by `cloud-agent onboard`. Key variables:

| Variable | Description |
|----------|-------------|
| `PARTY_AGENT` | Your Canton party ID |
| `PARTY_AGENT_PRIVATE_KEY` | Base58-encoded Ed25519 private key (32-byte seed) |
| `ORDERBOOK_GRPC_URL` | Orderbook gRPC endpoint |
| `SYNCHRONIZER_ID` | Canton synchronizer ID |
| `NODE_NAME` | Canton node name for routing |
| `LEDGER_SERVICE_PUBLIC_KEY` | Base58-encoded public key of the ledger service (for message verification) |
| `PARTY_SETTLEMENT_OPERATOR` | Settlement operator party ID |
| `PARTY_ORDERBOOK_FEE` | Fee collection party ID |
| `PARTY_TRAFFIC_FEE` | Traffic fee party ID |
| `TRAFFIC_FEE_PRICE_USD_MB` | Traffic fee rate in USD per MB |
| `JOIN_TRAFFIC_TRANSACTIONS` | Join traffic fee transactions (default: `true`) |
| `AGENT_FEE_RESERVE_CC` | Canton Coin reserve for fees (default: `5.0`) |
| `SETTLEMENT_THREAD_COUNT` | Concurrent settlement threads (default: `5`) |
| `AGENT_MAX_SETTLEMENTS` | Max active settlements (default: `10`) |

### `configuration.toml` — Token Configuration

Shared token registry and Canton Coin configuration:

```toml
[[registry]]
party = "registry-party-id..."
description = "Token Registry"

[[canton_coin]]
token_id = "canton-coin-token-id..."
dso_party = "dso-party-id..."
description = "Canton Coin"
```

### `agent.toml` — Agent Settings

Full example for a liquidity provider with grid orders and RFQ:

```toml
role = "agent"
auto_settle = true
poll_interval_secs = 10
token_ttl_secs = 3600
connection_timeout_secs = 30

# Liquidity provider identity (enables RFQ handling)
[liquidity_provider]
name = "My LP"
max_concurrent_rfqs = 10
default_quote_valid_secs = 30

# Market: CBTC/CC
[[markets]]
market_id = "market-id-here"
enabled = true
base_order_size = "0.001"
price_change_threshold_percent = 0.5

# Grid bid levels (below mid price)
[[markets.bid_levels]]
delta_percent = 0.5
quantity = "0.001"

[[markets.bid_levels]]
delta_percent = 1.0
quantity = "0.002"

[[markets.bid_levels]]
delta_percent = 2.0
quantity = "0.005"

# Grid offer levels (above mid price)
[[markets.offer_levels]]
delta_percent = 0.5
quantity = "0.001"

[[markets.offer_levels]]
delta_percent = 1.0
quantity = "0.002"

[[markets.offer_levels]]
delta_percent = 2.0
quantity = "0.005"

# RFQ configuration for this market
[markets.rfq]
enabled = true
min_quantity = "0.001"
max_quantity = "1.0"
bid_spread_percent = 0.5
offer_spread_percent = 0.5
quote_valid_secs = 30
allocate_before_secs = 3600
settle_before_secs = 7200
```

## Grid Orders

The agent places limit orders on a grid of price levels around the current mid price.

Each level is defined by:
- **`delta_percent`** — offset from mid price (e.g., `1.0` means 1% below mid for bids, 1% above for offers)
- **`quantity`** — order size at this level

When the mid price moves by more than `price_change_threshold_percent`, all grid orders are cancelled and re-placed at updated levels.

Example with mid price at 100,000:

| Level | delta_percent | Price | Side |
|-------|--------------|-------|------|
| Bid 1 | 0.5 | 99,500 | Buy |
| Bid 2 | 1.0 | 99,000 | Buy |
| Bid 3 | 2.0 | 98,000 | Buy |
| Offer 1 | 0.5 | 100,500 | Sell |
| Offer 2 | 1.0 | 101,000 | Sell |
| Offer 3 | 2.0 | 102,000 | Sell |

## RFQ (Request for Quote)

When `[liquidity_provider]` is configured, the agent connects to the orderbook server via a bidirectional gRPC stream and receives RFQ requests in real time.

### Flow

1. Agent connects and sends a handshake identifying the LP
2. Server routes incoming RFQ requests to the LP
3. Agent computes a quote price based on mid price and configured spread:
   - **Buy request** (user buys, LP sells): `price = mid * (1 + offer_spread_percent / 100)`
   - **Sell request** (user sells, LP buys): `price = mid * (1 - bid_spread_percent / 100)`
4. Agent validates quantity against `min_quantity` / `max_quantity`
5. Agent responds with a quote (including price, quantity, validity window, and settlement deadlines) or a rejection

### Rejection Reasons

The agent rejects an RFQ if:
- Market is not configured or disabled
- RFQ is not enabled for the market
- Quantity is below `min_quantity` or above `max_quantity`
- No mid-price is available yet (temporary — resolves after first price poll)

### Settlement Deadlines

Each quote includes:
- **`allocate_before_secs`** — seconds from DVP creation to allocate tokens (default: 3600 = 1 hour)
- **`settle_before_secs`** — seconds from DVP creation to complete settlement (default: 7200 = 2 hours)

## Running the Agent

Long-running mode — places grid orders, handles RFQ requests, and settles trades automatically:

```bash
cloud-agent agent
```

### Flags

| Flag | Description |
|------|-------------|
| `--settlement-only` | Disable order placement, only settle existing trades |
| `--orders-only` | Disable settlement, only place/manage grid orders |
| `--dry-run` | Prepare and verify transactions without signing or executing |
| `--confirm` | Prompt for confirmation before signing each transaction |
| `--verbose` | Enable verbose logging |
| `--config <PATH>` | Path to agent.toml (default: `agent.toml`) |

### Buyer / Seller Fill Loops

Execute a large trade in chunks via RFQ:

```bash
# Buy 10 units, chunked into 0.5-5.0 per settlement
cloud-agent buyer --market <ID> --amount 10 --min-settlement 0.5 --max-settlement 5.0

# Sell 10 units with price floor
cloud-agent seller --market <ID> --amount 10 --price-limit 95000
```

## CLI Reference

| Command | Description |
|---------|-------------|
| `cloud-agent agent` | Run as long-lived agent (orders + settlement + RFQ) |
| `cloud-agent onboard --rpc <URL>` | Self-service onboarding |
| `cloud-agent generate-private-key` | Generate a new Ed25519 keypair |
| `cloud-agent info balance` | Show token balances |
| `cloud-agent info party` | Show party ID, public key, node name |
| `cloud-agent info network` | Show DSO rates and mining rounds |
| `cloud-agent info list [--template <ID>] [--count]` | List active contracts |
| `cloud-agent transfer send-cc --receiver <ID> --amount <AMT>` | Send Canton Coin |
| `cloud-agent transfer send-cip56 --receiver <ID> --instrument-id <NAME> --instrument-admin <ID> --amount <AMT>` | Send CIP-56 token |
| `cloud-agent transfer accept-cip56 --contract-id <CID>` | Accept incoming CIP-56 transfer |
| `cloud-agent preapproval request` | Request a TransferPreapproval |
| `cloud-agent preapproval fetch` | List existing preapprovals |
| `cloud-agent subscription request-prepaid` | Request prepaid recurring payment |
| `cloud-agent subscription request-payasyougo` | Request pay-as-you-go subscription |
| `cloud-agent user-service request` | Request UserService (one-time onboarding) |
| `cloud-agent buyer --market <ID> --amount <AMT>` | Buy via RFQ fill loop |
| `cloud-agent seller --market <ID> --amount <AMT>` | Sell via RFQ fill loop |
| `cloud-agent sign multihash --input <B64>` | Sign a Canton multihash |
| `cloud-agent sign message --input <TEXT>` | Sign a text message |

## gRPC API

The agent communicates with the Silvana orderbook server via 4 gRPC services defined in `proto/silvana/*/v1/*.proto`.

### Services Overview

| Service | Proto | Role | Streaming |
|---------|-------|------|-----------|
| DAppProviderService | `ledger/v1/ledger.proto` | Two-phase transaction signing, balance and contract queries | Server-streaming (`GetActiveContracts`, `GetUpdates`) |
| SettlementService | `settlement/v1/settlement.proto` | DVP settlement orchestration, RFQ handling | Bidirectional (`SettlementStream`) |
| OrderbookService | `orderbook/v1/orderbook.proto` | Order submission, market data, RFQ initiation | Server-streaming (`SubscribeOrderbook`, `SubscribeOrders`, `SubscribeSettlements`) |
| PricingService | `pricing/v1/pricing.proto` | External price feeds (Binance, ByBit, CoinGecko) | Server-streaming (`StreamPrices`) |

### Two-Phase Transaction Flow

All ledger-mutating operations use a two-phase signing protocol:

1. Agent calls `PrepareTransaction` with an operation type and parameters
2. Server returns the full `prepared_transaction` bytes and a hash
3. Agent uses `tx-verifier` to independently verify the transaction matches the requested operation (correct template, parties, amounts)
4. Agent signs the hash locally with its Ed25519 private key
5. Agent calls `ExecuteTransaction` with the signature
6. Server submits the signed transaction to the Canton ledger

Operation types:

| Operation | Description |
|-----------|-------------|
| `TRANSFER_CC` | Send Canton Coin |
| `TRANSFER_CIP56` | Send CIP-56 token |
| `ACCEPT_CIP56` | Accept incoming CIP-56 transfer |
| `PAY_DVP_FEE` | Pay DVP processing fee |
| `PROPOSE_DVP` | Create DVP proposal |
| `ACCEPT_DVP` | Accept DVP proposal |
| `PAY_ALLOC_FEE` | Pay allocation processing fee |
| `ALLOCATE` | Allocate tokens to DVP |
| `REQUEST_PREAPPROVAL` | Request TransferPreapproval |
| `REQUEST_RECURRING_PREPAID` | Request prepaid subscription |
| `REQUEST_RECURRING_PAYASYOUGO` | Request pay-as-you-go subscription |
| `REQUEST_USER_SERVICE` | Request UserService (onboarding) |

### Settlement Stream (Bidirectional)

The `SettlementStream` RPC is a long-lived bidirectional gRPC stream used for RFQ handling and settlement lifecycle coordination.

**Agent sends:**
- Handshake (party ID, operator party, LP name)
- Heartbeats
- Preconfirmation decisions (accept/reject)
- DVP lifecycle events (creation, acceptance, allocation, settlement)
- RFQ quotes or rejections

**Server sends:**
- Handshake acknowledgement
- Heartbeats
- Settlement proposals
- Preconfirmation requests
- RFQ requests (routed from users requesting quotes)

The agent opens this stream when `[liquidity_provider]` is configured in `agent.toml`. RFQ requests arrive on this stream and the agent responds with a quote (price computed from mid-price and configured spread) or a rejection.

### Key Query RPCs

| RPC | Service | Returns |
|-----|---------|---------|
| `GetBalances` | Ledger | Token balances (total, locked, unlocked) |
| `GetActiveContracts` | Ledger | Stream of active contracts by template filter |
| `GetUpdates` | Ledger | Stream of ledger transactions from a given offset |
| `GetPrice` | Pricing | Bid, ask, last price for a market |
| `GetKlines` | Pricing | OHLCV candlestick data (1m to 1w intervals) |
| `GetOrders` | Orderbook | Orders with status and type filters |
| `GetOrderbookDepth` | Orderbook | Aggregated bid/offer price levels |
| `GetSettlementProposals` | Orderbook | Settlement proposals with status filter |
| `GetSettlementStatus` | Settlement | Step-by-step DVP status with buyer/seller next actions |
| `GetMarkets` | Orderbook | Available markets and their configuration |
| `GetMarketData` | Orderbook | Best bid/ask, last price, 24h volume |

### Server-Streaming Subscriptions

| RPC | Service | Payload |
|-----|---------|---------|
| `GetActiveContracts` | Ledger | Active contracts matching template filter |
| `GetUpdates` | Ledger | Ledger transaction stream from a given offset |
| `SubscribeSettlements` | Orderbook | Settlement status change events |
| `StreamPrices` | Pricing | Real-time price ticks with optional orderbook and trade data |

The agent currently uses polling (`poll_interval_secs`, default 10s) for mid-price updates rather than `StreamPrices`. Developers can modify this to use streaming for lower latency. `SubscribeOrderbook` and `SubscribeOrders` are also available but not used by the agent.

### Authentication

- **JWT** — Self-describing Ed25519 JWT (RFC 8037) with automatic refresh. Used for Orderbook, Pricing, and Settlement RPCs. The token embeds the Ed25519 public key; the server verifies it matches the registered party.
- **Message signing** — Per-request Ed25519 signature on the canonical request payload, used for Ledger two-phase transactions. Server responses are also signed; the agent verifies them using `LEDGER_SERVICE_PUBLIC_KEY`.

## Security Model

- **Two-phase signing**: The server prepares a transaction and returns the full prepared transaction bytes along with a hash. The agent independently recomputes the hash from the prepared transaction, verifies the transaction matches the expected operation using `tx-verifier`, and only then signs and submits.
- **Transaction verification**: Before signing, `tx-verifier` checks that the prepared transaction matches the requested operation (correct template, parties, amounts). This prevents the server from tricking the agent into signing unexpected transactions.
- **Ed25519 JWT authentication**: The agent creates self-describing JWTs with an embedded Ed25519 public key (RFC 8037). The server verifies the public key fingerprint matches the party ID.
- **Private key isolation**: The Ed25519 private key is only used locally for signing transaction hashes and JWTs. It is never transmitted to the server.

## License

See [LICENSE](LICENSE) for details.
