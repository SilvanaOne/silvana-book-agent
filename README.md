# Silvana Book Agent

Cloud agent for liquidity providers and takers on the Silvana orderbook. Runs without direct Canton ledger access — all ledger operations are proxied through the Silvana gRPC service. Your Ed25519 private key never leaves the agent; transactions are signed locally using a two-phase protocol.

This repo ships both a CLI (`cloud-agent`) and a set of Rust library crates you can import to build your own agents.

## Quick Start

### Install (prebuilt binary)

Download the latest `cloud-agent` binary from the
[GitHub Releases page](https://github.com/SilvanaOne/silvana-book-agent/releases/latest):

| Platform            | Archive                            |
| ------------------- | ---------------------------------- |
| macOS Apple Silicon | `cloud-agent-macos-silicon.tar.gz` |
| Linux x86_64        | `cloud-agent-x86_64-linux.tar.gz`  |
| Linux ARM64         | `cloud-agent-arm64-linux.tar.gz`   |

```bash
# example: macOS Apple Silicon
curl -LO https://github.com/SilvanaOne/silvana-book-agent/releases/latest/download/cloud-agent-macos-silicon.tar.gz
tar -xzf cloud-agent-macos-silicon.tar.gz
chmod +x cloud-agent
./cloud-agent --help
```

Checksums for all archives are in `checksums.txt` on the release page.

### Build from source

```bash
cargo build --release -p cli
```

Binary at `target/release/cloud-agent`.

### Onboard

Self-service onboarding generates an Ed25519 keypair, registers on the waiting list, signs the Canton topology transaction, creates preapprovals, requests a `UserService`, and (on devnet) auto-faucets initial CC + USDC balances. On success, it writes a `.env` (Canton party ID, private key, network parties, fee config) and a starter `agent.toml` (role, poll interval, empty `[[markets]]`) into the current directory. The command is idempotent — re-run safely; existing keys and party IDs are preserved.

```bash
cloud-agent onboard \
  --agent-name my-agent \
  --email me@example.com \
  --invite-code INVITE1
```

Default RPC is `https://orderbook-devnet.silvana.dev:443` — override with `--rpc`.

### Faucet (devnet top-up)

```bash
cloud-agent faucet get --token CC      # 10,000 CC
cloud-agent faucet get --token USDC    # 1,000 USDC
cloud-agent info balance
```

### Buy CC via RFQ

Requests a quote on the `CC-USDC` market, accepts the best offer, and settles atomically via a single multicall. Repeats until the full amount is filled.

```bash
cloud-agent buy --market CC-USDC --amount 15
```

### Sell CC via RFQ

```bash
cloud-agent sell --market CC-USDC --amount 100
```

### Run as LP / market-maker

Long-running mode: places grid orders on configured markets, responds to incoming RFQs, and settles DVPs end-to-end. Configured via `agent.toml` (see [Reference](#configuration) below).

```bash
cloud-agent agent
```

## SDK — Build Your Own Agent

The same crates the CLI uses are available as Rust libraries. Import them from path or git and build custom agents, batch tools, or test harnesses.

### Minimal example

The canonical minimal agent lives at [`examples/buy_cc`](examples/buy_cc). It buys CC on `CC-USDC` in ~120 lines — polling for quotes at a configurable interval until filled.

`examples/buy_cc/Cargo.toml`:

```toml
[package]
name = "buy-cc-example"
version = "0.1.0"
edition = "2024"

[dependencies]
cloud-agent = { path = "../../crates/cloud-agent" }
agent-logic = { path = "../../crates/agent-logic" }
orderbook-proto = { path = "../../crates/orderbook-proto" }
tokio = { version = "1", features = ["full"] }
anyhow = "1.0"
clap = { version = "4", features = ["derive"] }
dotenvy = "0.15"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }
```

`examples/buy_cc/src/main.rs` (abridged):

```rust
use std::sync::Arc;
use anyhow::{Context, Result};
use clap::Parser;

use agent_logic::config::BaseConfig;
use agent_logic::confirm::new_confirm_lock;
use agent_logic::liquidity::LiquidityManager;
use cloud_agent::accept_settle::MulticallSettler;
use cloud_agent::backend::CloudSettlementBackend;
use cloud_agent::fill_loop::{self, FillDirection, FillParams};
use cloud_agent::populate_instruments;

#[derive(Parser)]
struct Args {
    #[arg(long)] amount: f64,
    #[arg(long)] max_price: Option<f64>,
    #[arg(long, default_value = "600")] poll_period: u64,
    #[arg(long, default_value = "5.0")] min_settlement: f64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let args = Args::parse();
    tracing_subscriber::fmt().with_env_filter("info").init();

    // 1. Load config from .env + agent.toml
    let mut config = BaseConfig::load_or_defaults("agent.toml")?;

    // 2. Fetch instrument registry (CC/USDC/…) from orderbook-rpc
    populate_instruments(&mut config).await?;

    // 3. Create backend (spawns ACS worker to keep amulet cache fresh)
    let confirm_lock = new_confirm_lock();
    let lm = LiquidityManager::new(
        config.fee_reserve_cc, config.liquidity_margin,
        config.flow_ema_window_hours,
        config.depletion_max_hours, config.depletion_min_hours,
    );
    let backend = CloudSettlementBackend::new(
        config.clone(), false, false, false, false, confirm_lock.clone(), lm,
    );

    // 4. Create settler for atomic multicall settlement
    let settler = Arc::new(MulticallSettler {
        config: config.clone(),
        amulet_cache: backend.amulet_cache().clone(),
        verbose: false, dry_run: false, force: false, confirm: false,
        confirm_lock,
    });

    // 5. Run the fill loop
    let params = FillParams {
        direction: FillDirection::Buy,
        market_id: "CC-USDC".to_string(),
        total_amount: args.amount,
        price_limit: args.max_price,
        min_settlement: args.min_settlement,
        max_settlement: args.amount,
        interval_secs: args.poll_period,
    };
    let _backend_guard = backend;
    fill_loop::run_fill_loop(config, settler, params, None, None).await
}
```

Run it:

```bash
cargo run -p buy-cc-example -- --amount 10.0 --max-price 0.16 --poll-period 600
```

### What the SDK gives you

| Crate             | Key exports                                                                                                                                                                                           |
| ----------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `agent-logic`     | `BaseConfig`, `SettlementBackend` trait, `BalanceProvider` trait, `run_agent()`, `LiquidityManager`, `OrderbookClient`, `OrderbookRpcClient`, `OrderTracker`, `SettlementExecutor`, state persistence |
| `cloud-agent`     | `CloudSettlementBackend` (impls `SettlementBackend`), `DAppProviderClient`, `MulticallSettler`, `RfqHandler`, `PaymentQueue`, `AmuletCache`, `fill_loop::run_fill_loop`, onboarding helpers           |
| `orderbook-proto` | Generated gRPC clients + all protobuf types (`Market`, `Order`, `SettlementProposal`, `TokenBalance`, …) and reflection descriptor pools                                                              |
| `message-signing` | Canonical Ed25519 signing for `DAppProviderService` RPCs (`sign_canonical`, `verify_canonical`, canonical payload builders per operation type)                                                        |
| `tx-verifier`     | `verify_and_hash()` — independent Canton transaction verification + 3-layer SHA-256 hashing before signing                                                                                            |

### Patterns

- **Taker / one-shot trades** — use `fill_loop::run_fill_loop` with `CloudSettlementBackend` + `MulticallSettler` (like the example above).
- **Long-running LP / market-maker** — implement or configure `cloud_agent::run_cloud_agent(...)` directly, or mirror its setup: build a `CloudSettlementBackend`, a `CloudBalanceProvider`, and call `agent_logic::runner::run_agent()` with your own `AgentOptions`.
- **Custom backend** — implement the `SettlementBackend` trait on your own type (e.g. for a local Canton ledger or a different proxy) and reuse the entire settlement state machine from `agent-logic`.
- **One-off operations** — use `DAppProviderClient` directly for preapprovals, transfers, faucet requests, CIP-56 settlements, locks, etc.

## Reference

### Prerequisites

- [Rust](https://rustup.rs/) (stable toolchain)
- Protocol Buffers compiler: `apt install protobuf-compiler` (Linux) or `brew install protobuf` (macOS)
- Silvana orderbook RPC endpoint URL and an invite code (provided by Silvana)

### Onboarding (full detail)

The `onboard` command performs 11 steps automatically:

1. Load or generate an Ed25519 keypair (written to `.env` as `PARTY_AGENT_PRIVATE_KEY`).
2. Connect to the orderbook RPC (raw gRPC, no auth yet).
3. Fetch server configuration (`GetAgentConfig`) → write `.env` and seed `agent.toml`.
4. Register on the waiting list (`RegisterAgent`, signed).
5. Poll for `SIGNATURE_REQUIRED` status.
6. Fetch and sign the Canton topology multihash.
7. Submit the signature (`SubmitOnboardingSignature`).
8. Poll until `TOPOLOGY_CREATED`; write `PARTY_AGENT` to `.env`.
9. Create Splice `TransferPreapproval` for CC (pending operator acceptance).
10. Create one CIP-56 `TransferPreapproval` per non-DSO registrar discovered from `GetInstruments`.
11. Request a `UserService` and (on devnet) auto-faucet CC + USDC.

Flags:

| Flag                     | Description                                      |
| ------------------------ | ------------------------------------------------ |
| `--rpc <URL>`            | Orderbook gRPC endpoint (default devnet)         |
| `--agent-name <NAME>`    | Display name (required)                          |
| `--email <EMAIL>`        | Contact email (required)                         |
| `--invite-code <CODE>`   | Waiting list invite code (required)              |
| `--party <ID>`           | Skip waiting list (requires `--private-key`)     |
| `--private-key <B58>`    | Base58-encoded Ed25519 private key               |
| `--env-file <PATH>`      | Path to .env file (default: `.env`)              |
| `--poll-interval <SECS>` | Polling interval during onboarding (default: 10) |

### Configuration

`.env` and `agent.toml` are both created by `cloud-agent onboard` — you typically don't write them from scratch. The tables below describe each field so you can tune an existing config.

#### `.env` — Environment Variables

| Variable                         | Description                                                            | Written by `onboard` |
| -------------------------------- | ---------------------------------------------------------------------- | :------------------: |
| `PARTY_AGENT`                    | Your Canton party ID                                                   |         yes          |
| `PARTY_AGENT_PRIVATE_KEY`        | Base58 Ed25519 private key (32-byte seed)                              |         yes          |
| `PARTY_AGENT_PUBLIC_KEY`         | Base58 Ed25519 public key (derived from the private key)               |         yes          |
| `ORDERBOOK_GRPC_URL`             | Orderbook gRPC endpoint                                                |         yes          |
| `CANTON_CHAIN`                   | `devnet` \| `testnet` \| `mainnet`                                     |         yes          |
| `SYNCHRONIZER_ID`                | Canton synchronizer ID                                                 |         yes          |
| `NODE_NAME`                      | Canton node name for routing                                           |         yes          |
| `LEDGER_SERVICE_PUBLIC_KEY`      | Base58 public key of the ledger service (for verifying responses)      |         yes          |
| `DSO`                            | DSO (Canton Coin admin) party ID                                       |         yes          |
| `PARTY_SETTLEMENT_OPERATOR`      | Settlement operator party ID                                           |         yes          |
| `PARTY_ORDERBOOK_FEE`            | Orderbook fee collection party                                         |         yes          |
| `PARTY_TRAFFIC_FEE`              | Sequencer traffic fee party                                            |         yes          |
| `TRAFFIC_FEE_PRICE_USD_MB`       | Traffic fee rate in USD per MB                                         |         yes          |
| `JOIN_TRAFFIC_TRANSACTIONS`      | Batch traffic-fee transactions (default: `true`)                       |         yes          |
| `AGENT_FEE_RESERVE_CC`           | CC balance held back for fees (default: `5.0`)                         |         yes          |
| `AGENT_FEE_CC`                   | Per-tx agent fee (CC)                                                  |         yes          |
| `PARTICIPANT_FEE_CC`             | Per-tx participant fee (CC)                                            |         yes          |
| `SIGNATURE_FEE_CC`               | Per-tx signature fee (CC)                                              |         yes          |
| `MERGE_THRESHOLD`                | Merge worker triggers when selectable amulets exceed this count        |         yes          |
| `MERGE_MAX_AMULETS`              | Max amulets merged per round (default: `100`)                          |         yes          |
| `MERGE_POLL_INTERVAL_SEC`        | Merge worker poll interval in seconds (default: `600`)                 |         yes          |
| `LOG_DESTINATION`                | `console` \| `file` (paired with `LOG_DIR`, `LOG_FILE_PREFIX`)         |         yes          |
| `RECURRING_PAYMENT_PACKAGE_NAME` | Recurring-payment package name; required for `subscription *` commands |         yes          |
| `SETTLEMENT_THREAD_COUNT`        | Concurrent settlement threads (default used when unset)                |          no          |
| `AGENT_MAX_SETTLEMENTS`          | Max active settlements (default used when unset)                       |          no          |

#### `agent.toml` — Agent Settings

Full example for an LP with grid orders and RFQ:

```toml
role = "trader"
auto_settle = true
poll_interval_secs = 7
token_ttl_secs = 3600
connection_timeout_secs = 30

# Liquidity provider identity (enables RFQ handling)
[liquidity_provider]
name = "My LP"
max_concurrent_rfqs = 10
default_quote_valid_secs = 60

[[markets]]
market_id = "CC-USDC"
enabled = true
price_change_threshold_percent = 0.1

# 3 bids below mid (negative deltas)
[[markets.bid_levels]]
delta_percent = -0.015
quantity = "10"
[[markets.bid_levels]]
delta_percent = -0.2
quantity = "10"
[[markets.bid_levels]]
delta_percent = -0.3
quantity = "10"

# 3 offers above mid (positive deltas)
[[markets.offer_levels]]
delta_percent = 0.015
quantity = "10"
[[markets.offer_levels]]
delta_percent = 0.2
quantity = "10"
[[markets.offer_levels]]
delta_percent = 0.3
quantity = "10"

# RFQ configuration for this market
[markets.rfq]
enabled = true
min_quantity = "5"
max_quantity = "1000"
bid_spread_percent = 0.5
offer_spread_percent = 0.5
quote_valid_secs = 60
allocate_before_secs = 3600
settle_before_secs = 7200
```

### Grid Orders

When `agent` runs, it places limit orders on a grid of price levels around the current mid price.

Each level is defined by:

- **`delta_percent`** — signed offset from mid price in percent. Bids use negative values (below mid), offers use positive values (above mid). E.g. `-0.5` on a bid = 0.5% below mid; `0.3` on an offer = 0.3% above mid.
- **`quantity`** — order size at this level

When the mid price moves by more than `price_change_threshold_percent`, all grid orders are cancelled and re-placed at updated levels.

### RFQ (Request for Quote)

When `[liquidity_provider]` is configured, the agent connects to the orderbook server via a bidirectional gRPC stream (`SettlementStream`) and receives RFQ requests in real time.

Flow:

1. Agent connects and sends a handshake identifying the LP.
2. Server routes incoming RFQ requests to the LP.
3. Agent computes a quote price from the mid price and configured spread:
   - **Buy request** (user buys, LP sells): `price = mid * (1 + offer_spread_percent / 100)`
   - **Sell request** (user sells, LP buys): `price = mid * (1 - bid_spread_percent / 100)`
4. Agent validates quantity against `min_quantity` / `max_quantity` and checks balance via `LiquidityManager`.
5. Agent responds with a quote (price, quantity, validity window, settlement deadlines) or a rejection.

Rejection reasons: market disabled, RFQ disabled for market, quantity out of range, no mid-price yet, insufficient balance.

Each quote includes:

- **`allocate_before_secs`** — seconds from DVP creation to allocate tokens (default: 3600)
- **`settle_before_secs`** — seconds from DVP creation to complete settlement (default: 7200)

### Taker Fill Loops (`buy` / `sell`)

Execute a large trade in chunks via repeated RFQ. Each accepted quote is settled atomically via a single multicall (`Accept_Dvp + Allocate + fees + traffic`).

```bash
# Buy 10 CC, chunked 0.5-5.0 per round, 60s interval between retries
cloud-agent buy --market CC-USDC --amount 10 \
  --min-settlement 0.5 --max-settlement 5.0 --interval 60

# Sell 10 CC with price floor
cloud-agent sell --market CC-USDC --amount 10 --price-limit 0.14
```

Flags (both `buy` and `sell`):

| Flag                   | Description                                                |
| ---------------------- | ---------------------------------------------------------- |
| `--market <ID>`        | Market ID (e.g. `CC-USDC`)                                 |
| `--amount <N>`         | Total amount to fill (base instrument)                     |
| `--price-limit <N>`    | Max (buy) or min (sell) price per unit — default: mid ± 3% |
| `--min-settlement <N>` | Minimum per-round size (default: 5.0)                      |
| `--max-settlement <N>` | Maximum per-round size (default: total amount)             |
| `--interval <SECS>`    | Retry interval when no quote arrives (default: 60)         |

### `agent` mode — flags

| Flag                | Description                                                  |
| ------------------- | ------------------------------------------------------------ |
| `--settlement-only` | Disable order placement, only settle existing trades         |
| `--orders-only`     | Disable settlement, only place/manage grid orders            |
| `--no-restore`      | Skip loading previous state from `agent-state.json`          |
| `--no-reject`       | Accept all proposals without RFQ-state verification          |
| `--dry-run`         | Prepare and verify transactions without signing or executing |
| `--force`           | Sign and execute even if verification fails                  |
| `--confirm`         | Prompt for confirmation before signing each transaction      |
| `--verbose`         | Enable verbose logging                                       |
| `--config <PATH>`   | Path to agent.toml (default: `agent.toml`)                   |

### Full CLI Reference

| Command                                                                                       | Description                                             |
| --------------------------------------------------------------------------------------------- | ------------------------------------------------------- |
| `agent`                                                                                       | Long-running LP agent (grid + RFQ + settlement)         |
| `onboard`                                                                                     | Self-service onboarding                                 |
| `generate-private-key`                                                                        | Generate a new Ed25519 keypair (no config needed)       |
| `info balance`                                                                                | Show token balances                                     |
| `info party`                                                                                  | Show party ID, public key, node name                    |
| `info network`                                                                                | Show DSO party, rates, mining rounds                    |
| `info list [--template ID] [--count]`                                                         | List active contracts                                   |
| `buy --market <ID> --amount <N>`                                                              | Buy via RFQ until filled                                |
| `sell --market <ID> --amount <N>`                                                             | Sell via RFQ until filled                               |
| `faucet get --token <CC\|USDC\|…> [--admin <P>] [--amount <N>]`                               | Request tokens (devnet)                                 |
| `transfer send-cc --receiver <P> --amount <N>`                                                | Send Canton Coin                                        |
| `transfer send-cip56 --receiver <P> --instrument-id <ID> --instrument-admin <P> --amount <N>` | Send CIP-56 token                                       |
| `transfer accept-cip56 --contract-id <CID>`                                                   | Accept incoming CIP-56 transfer                         |
| `transfer split-cc --output-amounts a,b,c --amulet-cids x,y`                                  | Split CC amulets                                        |
| `transfer batch-pay --file payments.csv`                                                      | Batch CC payments (atomic multicall)                    |
| `preapproval request --instrument-admin <P>`                                                  | Create a `TransferPreapproval`                          |
| `preapproval fetch`                                                                           | List existing preapprovals                              |
| `subscription request-prepaid`                                                                | Request a prepaid recurring payment                     |
| `subscription request-payasyougo`                                                             | Request a pay-as-you-go subscription                    |
| `user-service request`                                                                        | Request a `UserService` contract (one-time onboarding)  |
| `lock holdings …`                                                                             | Lock holdings via `LockService.LockHoldings`            |
| `lock vote …`                                                                                 | Process voting lock/unlock requests on `LockController` |
| `lock resize …`                                                                               | Resize lock amount on `LockController`                  |
| `lock terminate …`                                                                            | Terminate lock on `LockController`                      |
| `sign multihash --input <B64>`                                                                | Sign a Canton 34-byte multihash                         |
| `sign message --input <TEXT>`                                                                 | Sign a UTF-8 text message                               |
| `sign binary --input <HEX>`                                                                   | Sign hex-encoded binary data                            |

### gRPC API

The agent communicates with the Silvana orderbook server via 4 gRPC services defined in `proto/silvana/*/v1/*.proto`.

| Service               | Proto                            | Role                                                        | Streaming                                                                          |
| --------------------- | -------------------------------- | ----------------------------------------------------------- | ---------------------------------------------------------------------------------- |
| `DAppProviderService` | `ledger/v1/*.proto`              | Two-phase transaction signing, balance and contract queries | Server-streaming (`GetActiveContracts`, `GetUpdates`)                              |
| `SettlementService`   | `settlement/v1/settlement.proto` | DVP settlement orchestration, RFQ handling                  | Bidirectional (`SettlementStream`)                                                 |
| `OrderbookService`    | `orderbook/v1/orderbook.proto`   | Order submission, market data, RFQ initiation               | Server-streaming (`SubscribeOrderbook`, `SubscribeOrders`, `SubscribeSettlements`) |
| `PricingService`      | `pricing/v1/pricing.proto`       | External price feeds (Binance, ByBit, CoinGecko)            | Server-streaming (`StreamPrices`)                                                  |

#### Two-Phase Transaction Flow

All ledger-mutating operations use a two-phase signing protocol:

1. Agent calls `PrepareTransaction` with an operation type and parameters.
2. Server returns the full `prepared_transaction` bytes and a hash.
3. Agent uses `tx-verifier` to independently verify the transaction matches the requested operation (correct template, parties, amounts).
4. Agent signs the hash locally with its Ed25519 private key.
5. Agent calls `ExecuteTransaction` with the signature.
6. Server submits the signed transaction to the Canton ledger.

Supported operation types include: `TRANSFER_CC`, `TRANSFER_CIP56`, `ACCEPT_CIP56`, `PAY_DVP_FEE`, `PROPOSE_DVP`, `ACCEPT_DVP`, `PAY_ALLOC_FEE`, `ALLOCATE`, `EXECUTE_MULTICALL`, `SPLIT_CC`, `REQUEST_PREAPPROVAL`, `REQUEST_RECURRING_PREPAID`, `REQUEST_RECURRING_PAYASYOUGO`, `REQUEST_USER_SERVICE`, `LOCK_HOLDINGS`, `PROCESS_LOCK_UNLOCK_REQUESTS`, `RESIZE_LOCK`, `TERMINATE_LOCK`.

#### Settlement Stream (Bidirectional)

The `SettlementStream` RPC is a long-lived bidirectional gRPC stream used for RFQ handling and settlement lifecycle coordination.

**Agent sends:** handshake, heartbeats, preconfirmations, DVP lifecycle events, RFQ quotes/rejections.

**Server sends:** handshake ack, heartbeats, settlement proposals, preconfirmation requests, RFQ requests.

The agent opens this stream when `[liquidity_provider]` is configured in `agent.toml`.

#### Authentication

- **JWT** — Self-describing Ed25519 JWT (RFC 8037) with automatic refresh. Used for Orderbook, Pricing, and Settlement RPCs.
- **Message signing** — Per-request Ed25519 signature on the canonical request payload, used for `DAppProviderService` two-phase transactions. Server responses are also signed and the agent verifies them using `LEDGER_SERVICE_PUBLIC_KEY`.

### Security Model

- **Two-phase signing** — The server prepares a transaction and returns the bytes and a hash. The agent independently recomputes the hash, verifies the transaction with `tx-verifier`, and only then signs.
- **Transaction verification** — Before signing, `tx-verifier` checks that the prepared transaction matches the requested operation (correct template, parties, amounts). This prevents the server from tricking the agent into signing unexpected transactions.
- **Ed25519 JWT authentication** — Self-describing JWTs embed the Ed25519 public key (RFC 8037); the server verifies the public key fingerprint matches the party ID.
- **Private key isolation** — The Ed25519 private key is only used locally for signing transaction hashes and JWTs. It is never transmitted to the server.
