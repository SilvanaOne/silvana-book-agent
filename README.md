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

Self-service onboarding generates an Ed25519 keypair, registers on the waiting list, signs the Canton topology transaction, creates preapprovals, requests a `UserService`, and (on devnet) auto-faucets an initial balance of every token the ledger service advertises. On success, it writes a `.env` (Canton party ID, private key, network parties, fee and prepaid-traffic config) and, unless `agent.toml` already exists, the starter `agent.toml` served by the ledger service into the current directory. The command can be re-run; an existing party ID and key in `.env` are reused. A key passed with `--private-key` or entered at the prompt is not written to `.env`: later commands for that identity read it from `--private-key`, from `PARTY_AGENT_PRIVATE_KEY` in the environment, or from a terminal prompt.

Check the starter `agent.toml` before running `agent`: a `[liquidity_provider]` section runs the agent as an LP (see [Run the agent](#run-the-agent-grid-trader-or-lp)). To use your own config, write `agent.toml` before onboarding; it is never overwritten.

```bash
cloud-agent onboard \
  --network devnet \
  --agent-name my-agent \
  --invite-code INVITE1
```

`--network` picks the rpc: `mainnet` (the default) is `https://orderbook-mainnet.silvana.dev:443`, `devnet` is `https://orderbook-devnet.silvana.dev:443`. `testnet` is refused before anything is written, because it has no rpc yet. Onboarding also refuses an rpc whose agent configuration names another `CANTON_CHAIN` than the chosen network. No email is sent: it comes from the account that owns the invite code, and an older command that still passes `--email` works, with a notice that the flag is ignored.

### Faucet (devnet top-up)

```bash
cloud-agent faucet get --token CC      # 10,000 CC
cloud-agent faucet get --token USDC    # 1,000 USDC
cloud-agent info balance
cloud-agent info prepaid-traffic       # off-chain balance that settlement fees are paid from
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

### Run the agent (grid trader or LP)

Long-running mode: places grid orders on configured markets, settles its order-book matches end-to-end and, as an LP, responds to incoming RFQs. Configured via `agent.toml` (see [Reference](#configuration) below).

```bash
cloud-agent agent
```

- **Grid trader** (no `[liquidity_provider]` section): settles each order-book match step by step, as DVP proposer or acceptor. It allocates once the counterparty's processing fees are confirmed.
- **LP** (`[liquidity_provider]` present): also answers RFQs, and allocates last, once the counterparty's fees and allocation are confirmed. By default the order book does not match two connected LPs against each other.

## Strategies

Without further settings every market runs the grid described under [Grid Orders](#grid-orders). A `[markets.strategy]` table picks another strategy for that market; parameters left out keep their defaults:

```toml
[[markets]]
market_id = "CC-USDC"

[markets.strategy]
kind = "market_maker"   # grid, market_maker, rebalancer, momentum, mean_reversion, noise_taker, twap, arbitrage
half_spread_bps = 12
```

Rules:

- A strategy other than `grid` cannot be combined with `bid_levels` / `offer_levels` on the same market; the agent refuses to start. `kind = "grid"` with levels is the same as having no table.
- An unknown `kind` or parameter, or a value out of range, stops the agent at startup.
- An agent that runs strategies must not have a `[liquidity_provider]` section: the order book does not match liquidity providers against each other, so such an agent would lose its main counterparty. The agent refuses to start. With `rfq_v2_only` the table is ignored.
- A strategy decides once per cycle. It pauses while the reference price is stale, has jumped more than 1% from the last reading, or is within 1% of `ref_floor` / `ref_cap`.
- Takers send immediate-or-cancel (IOC) orders at exactly the best price of another agent, and only when that price beats the liquidity provider's quote and lies within the strategy's slippage limit. A take is never larger than one order at that level, so it settles once, and no new take goes out while the last one is listed or settling. With `take_lp = true` a taker may also take the liquidity provider's best level when it is the best price; an agent level at the same price comes first. The same slippage limit applies, and such a take is at most one of the provider's orders at that level and never below the server's minimum order value.
- Makers rest orders inside the liquidity provider's quotes and re-quote when their target moves more than 2 bps.
- Takers, and the makers that rest a single order (`rebalancer`, `mean_reversion`), send nothing new while a settlement of theirs is open in that market; `max_inflight` limits only `market_maker`, and the other kinds accept it so configurations that set it still load.
- Position limits and the report's change count from the balances at the strategy's start; these are saved at a stop and kept if the agent restarts within 6 hours with the same kind on that market, otherwise taken again from the first usable balances. Funds moved in or out while the agent was stopped move the kept start by the same amount, with a warning, so only the position carries over.
- Amounts named `_usd` are in US dollars. They are converted to the market's quote at its USD rate: 1 for a USD stablecoin quote (USDC, USDCx, TUSDT, USD8), the live CC/USD rate for a CC quote. A CC-quoted strategy waits while that rate is unknown or more than 30 minutes old. Any other quote, for example cETH in EDELx-cETH or EDELx in HECTO-EDELx, is valued through a listed market that prices it: first the quote against a USD stablecoin (cETH-USDCx, cETH-USDC, cETH-TUSDT, ...), then against CC (converted at the CC/USD rate), then the inverse pairs (USDCx-cETH, ..., CC-cETH). The agent reads that market's last price every 30 seconds and uses it while the price is at most 5 minutes old; the rate market is only read, and its orders and grid are never touched. While no rate is known a strategy holds and a maker withdraws its quotes. If no listed market prices the quote, this is logged once and the strategy waits; the market list is checked again every 10 minutes.
- Orders are worth at least $12 and use round lots worth at most one quote unit and at most $1; rungs below the server's minimum order value are skipped (the grid skips them too). For a quote worth more than $1, the status line prints quote amounts to about a cent and also shows the value in US dollars.
- Every 5 minutes each enabled market, the grid included, logs one line: balances, value at the reference price, change since the start and against simply holding, takes (filled and empty), fills, and the strategy's own state.

Parameters every strategy except `grid` accepts:

| Parameter | Default | Meaning |
|---|---|---|
| `max_settlements_per_hour` | 120 | Settlements in a rolling hour that pause the strategy for 15 min; a second hit stops it until a restart |
| `ref_floor`, `ref_cap` | unset | Price bounds of the reference; readings within 1% of either are not traded on |
| `ref_max_age_secs` | 90 | Age of the reference price after which it is stale; makers keep their quotes for up to 5 min of staleness |
| `take_lp` | false | Takes may also hit the liquidity provider's best level (see the rules above). Every kind except `market_maker`; `rebalancer` and `mean_reversion` only with `mode = "taker"` |

### `market_maker`

Quotes a two-sided ladder around a fair price, `reference × (1 − skew × inventory ratio)`, where the ratio is the position since the start over `max_inventory_usd`. The first rung joins inside the liquidity provider's quote. The side that would grow the position shrinks, and is dropped at the limit. After a burst of fills the spread widens for 10 minutes.

| Parameter | Default | Meaning |
|---|---|---|
| `levels` | 2 | Rungs per side (1–5) |
| `half_spread_bps` | 10 | Distance of the first rung from the fair price |
| `level_step_bps` | 10 | Extra distance of each further rung |
| `level_usd` | 40 | Value of the first rung |
| `size_growth` | 1.5 | Size factor from one rung to the next |
| `max_inventory_usd` | 300 | Position at which the growing side is dropped |
| `skew_bps` | 15 | Fair-price shift at the position limit; below twice `half_spread_bps` |
| `lp_inside` | true | Move the first rung inside the liquidity provider's quote |
| `lp_improve_ticks` | 1 | Ticks inside that quote |
| `min_edge_bps` | 3 | Least distance from the reference when moving inside |
| `pull_jump_bps` | 30 | A reference move this large withdraws the ladder for 2 cycles |
| `max_inflight` | 8 | Open settlements at which nothing new is placed |
| `max_fills_per_hour` | 60 | Fills in an hour that widen the spread |
| `throttle_spread_bps` | 40 | Half spread while widened |

### `rebalancer`

Keeps the base share of the portfolio value near `target_share`. It starts trading when the share is `band_enter` away and stops inside `band_exit`, either taking or resting one order near the reference.

| Parameter | Default | Meaning |
|---|---|---|
| `target_share` | required | Base share of the portfolio value to hold (0.05–0.95) |
| `band_enter` | 0.02 | Share gap at which trading starts |
| `band_exit` | 0.005 | Share gap at which it stops |
| `mode` | `taker` | `taker` (IOC) or `maker` (one resting order) |
| `clip_usd` | 20 | Value of one trade |
| `cooldown_secs` | 150 | Least time between trades |
| `maker_offset_bps` | 3 | Distance of the resting order from the reference |
| `max_slip_bps` | 15 | Taker price limit from the reference |
| `max_usd_per_hour` | 600 | Value traded per rolling hour |
| `flip_cooldown_secs` | 900 | Wait after a fill before trading the other way |
| `max_inflight` | 1 (taker), 2 (maker) | Accepted, but nothing new is sent while a settlement is open |

### `momentum`

Follows a crossover of a fast and a slow time-based EMA. A signal above `enter_bps` moves it from flat to a long target, one below `-enter_bps` to a short target; it returns to flat under `exit_bps`, never straight from long to short. It reaches its target in clips, holds each position for `min_hold_secs`, and caps how many positions it opens per hour. It does not trade until the averages have warmed up, also after a restart.

| Parameter | Default | Meaning |
|---|---|---|
| `fast_secs` | 180 | Fast EMA time constant |
| `slow_secs` | 900 | Slow EMA time constant; at least 3 × `fast_secs` |
| `enter_bps` | 12 | Signal that opens a position |
| `exit_bps` | 4 | Signal under which a position closes; below `enter_bps` |
| `max_position_usd` | 200 | Size of the long or short target |
| `clip_usd` | 30 | Value of one take |
| `clip_interval_secs` | 20 | Least time between takes |
| `min_hold_secs` | 600 | Least time a position is held |
| `max_flips_per_hour` | 3 | Positions opened per rolling hour |
| `warmup_secs` | 600 | History needed before trading |
| `max_slip_bps` | 15 | Price limit from the reference |
| `max_inflight` | 2 | Accepted, but nothing new is sent while a settlement is open |

### `mean_reversion`

Fades moves away from an EMA: the target position grows from zero at `entry_bps` of deviation to `max_position_usd` at `full_bps`, against the move. A deviation beyond `stop_bps` is treated as a trend: it flattens and opens nothing new for `pause_after_stop_secs`. It rests one order near the reference or takes.

| Parameter | Default | Meaning |
|---|---|---|
| `ema_secs` | 1200 | EMA time constant |
| `entry_bps` | 15 | Deviation where a position starts |
| `full_bps` | 60 | Deviation where the position is full |
| `stop_bps` | 150 | Deviation treated as a trend |
| `max_position_usd` | 200 | Full position |
| `mode` | `maker` | `maker` (one resting order) or `taker` (IOC) |
| `maker_offset_bps` | 4 | Distance of the resting order from the reference |
| `clip_usd` | 30 | Value of one order |
| `warmup_secs` | 600 | History needed before trading |
| `pause_after_stop_secs` | 900 | Pause after a stop |
| `max_slip_bps` | 15 | Taker price limit from the reference |
| `max_inflight` | 2 | Accepted, but nothing new is sent while a settlement is open |

### `noise_taker`

Trades at random times (exponential gaps, clamped to a quarter and four times the mean) and random sizes, leaning toward the side that brings its position back to the start. Some arrivals rest a short GTD order instead of taking, so that two takers can also trade with each other.

| Parameter | Default | Meaning |
|---|---|---|
| `mean_interval_secs` | 60 | Mean time between arrivals |
| `clip_usd_min`, `clip_usd_max` | 15, 40 | Range of the trade value |
| `inventory_bias` | 0.4 | Lean toward the reducing side |
| `max_position_usd` | 250 | Past this only the reducing side trades |
| `max_slip_bps` | 20 | Price limit from the reference |
| `rest_probability` | 0.2 | Share of arrivals that rest a GTD order |
| `rest_ttl_secs` | 45 | Life of that order |
| `seed` | from the start time | Random seed; the seed is logged, so a run can be replayed |
| `max_inflight` | 2 | Accepted, but nothing new is sent while a settlement is open |

### `twap`

Buys or sells `total_quantity` over `duration_secs` in jittered slices. Each slice aims for where the schedule stands at its end: it rests a GTD order one tick inside the best price for `passive_secs`, then takes within the slippage limit. Progress counts its own fills and pending settlements, so it never trades past the total. Any shortfall is logged at the end, 10 minutes after the schedule, instead of chasing the price. With `start_at`, a restarted agent first reads its own settlements since that time and keeps to the same schedule. Not allowed on a market with `[markets.rfq]` enabled.

| Parameter | Default | Meaning |
|---|---|---|
| `side` | required | `buy` or `sell` |
| `total_quantity` | required | Base quantity, as a decimal string |
| `start_at` | unset | RFC 3339 start time; makes a restart resume |
| `start_delay_secs` | 300 | Start this long after launch when `start_at` is unset |
| `duration_secs` | 4800 | Schedule length |
| `slice_secs` | 240 | Mean time between slices |
| `passive_secs` | 60 | How long each slice rests before taking; below `slice_secs` |
| `max_slip_bps` | 15 | Taker price limit from the reference |
| `max_catchup` | 2.0 | Most nominal slices one slice may carry when behind |
| `time_jitter` | 0.2 | Random spread of the slice times |
| `size_jitter` | 0.15 | Random spread of the slice sizes |
| `seed` | from the start time | Random seed, logged |
| `max_inflight` | 3 | Accepted, but nothing new is sent while a settlement is open |

### `arbitrage`

Takes the best agent offer priced at least `min_edge_bps` under the reference, or the best agent bid as far above it, while the reference is fresh and its readings over the last 2 minutes stay within `ref_stable_bps`. It watches the reference for a minute before its first take, and again after a gap of more than 2 minutes between readings; past half its position limit it only reduces.

| Parameter | Default | Meaning |
|---|---|---|
| `min_edge_bps` | 6 | Least distance of the agent price through the reference |
| `clip_usd_max` | 100 | Largest take |
| `max_position_usd` | 150 | Position limit |
| `max_inflight` | 1 | Accepted, but nothing new is sent while a settlement is open |
| `cooldown_secs` | 10 | Least time between takes |
| `ref_max_age_secs` | 90 | Age of the reference price after which nothing is taken |
| `ref_stable_bps` | 30 | Largest spread of the recent readings |

### Example configurations

A market maker:

```toml
role = "trader"
auto_settle = true
poll_interval_secs = 5

[[markets]]
market_id = "CC-USDC"
enabled = true

[markets.strategy]
kind = "market_maker"
levels = 3
half_spread_bps = 15
level_step_bps = 15
level_usd = 60
max_inventory_usd = 500
skew_bps = 20
```

A taker, buying 2000 CC over an hour from a fixed start:

```toml
role = "trader"
auto_settle = true
poll_interval_secs = 5

[[markets]]
market_id = "CC-USDC"
enabled = true

[markets.strategy]
kind = "twap"
side = "buy"
total_quantity = "2000"
start_at = "2026-12-01T10:00:00Z"
duration_secs = 3600
slice_secs = 240
max_slip_bps = 15
```

**Fees.** Each settlement costs each side a processing fee that is nearly fixed, whatever the trade's size. Trades of a few tens of dollars, as with the defaults above, therefore lose money by design: the defaults suit testing and activity, not profit. Size trades well above the fee before running a strategy for its returns.

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

fn main() -> Result<()> {
    // Load .env before any thread starts: changing the environment later is unsound
    let _ = dotenvy::dotenv();
    let args = Args::parse();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("Failed to start the async runtime")?
        .block_on(run(args))
}

async fn run(args: Args) -> Result<()> {
    let _ = tracing_subscriber::fmt().with_env_filter("info").try_init();

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
        agent_logic::shutdown::Shutdown::new(),
        cloud_agent::holdings_cache::HoldingsCache::new(false),
    )?;

    // 4. Create settler for atomic multicall settlement
    let settler = Arc::new(MulticallSettler {
        config: config.clone(),
        amulet_cache: backend.amulet_cache().clone(),
        verbose: false, dry_run: false, force: false, confirm: false,
        confirm_lock, fee_debits: Default::default(),
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
        atomic: false,
        fee_tokens: vec![],
    };
    let _backend_guard = backend;
    fill_loop::run_fill_loop(config, settler, params, None, None, None).await
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

1. Load an Ed25519 key (from `--private-key`, a prompt, or `.env`) or generate one; only a generated key is written to `.env` as `PARTY_AGENT_PRIVATE_KEY`.
2. Connect to the orderbook RPC (raw gRPC, no auth yet).
3. Fetch server configuration (`GetAgentConfig`) → write `.env` and seed `agent.toml`; `PARTY_FEE` and `PARTY_PREPAID_TRAFFIC` are written only where none is set, and never replaced.
4. Register on the waiting list (`RegisterAgent`, signed).
5. Poll for `SIGNATURE_REQUIRED` status.
6. Fetch and sign the Canton topology multihash.
7. Submit the signature (`SubmitOnboardingSignature`).
8. Poll until `TOPOLOGY_CREATED`; write `PARTY_AGENT` to `.env`.
9. Create Splice `TransferPreapproval` for CC (pending featured-app acceptance).
10. Create one CIP-56 `TransferPreapproval` per non-DSO registry advertised by the ledger service (`ListFaucetInstruments`). Preapprovals are per registry **admin**, so instruments sharing a registry are covered by a single preapproval, and an admin you already hold one for is skipped.
11. Request a `UserService`; on devnet, auto-faucet every advertised token and make the first prepaid-traffic top-up.

Flags:

| Flag                     | Description                                      |
| ------------------------ | ------------------------------------------------ |
| `--network <NETWORK>`    | `mainnet` (default) or `devnet`; `testnet` is refused, it has no rpc yet |
| `--agent-name <NAME>`    | Display name (required)                          |
| `--invite-code <CODE>`   | Waiting list invite code (required)              |
| `--party <ID>`           | Skip the waiting list; prompts for the private key when none is configured |
| `--private-key <B58>`    | Base58 Ed25519 private key for `--party` (optional in a terminal — prompted, input hidden) |
| `--env-file <PATH>`      | Path to .env file (default: `.env`; global flag, works on every command) |
| `--config-dir <DIR>`     | Agent folder; onboard writes `.env` and `agent.toml` there (create the folder first; global flag, works on every command) |
| `--poll-interval <SECS>` | Polling interval during onboarding, 1–3600 (default: 10) |

### Configuration

`.env` and `agent.toml` are both created by `cloud-agent onboard` — you typically don't write them from scratch. The tables below describe each field so you can tune an existing config.

Every command accepts a global `--config-dir <DIR>` flag that runs it from one agent's own folder. The folder becomes the working directory, so `agent.toml`, `agent-state.json`, `backups/`, `net-positions.json`, `logs/` and every relative path (`-c` and `--env-file` included) resolve inside it, and its `.env` is the env file. One shared binary can run many agents this way, one folder each:

```bash
cloud-agent --config-dir agent1 info balance
cloud-agent --config-dir agent1 agent
cloud-agent --config-dir agent2 agent
```

The folder must exist, and the agent logs `Config folder: <path>` at startup. Its `.env` is loaded like an explicit `--env-file`: it overrides variables already set in the shell, a missing `.env` stops the command (except `onboard`, which creates `.env` and `agent.toml` in the folder), and parent folders are not searched. `--env-file <PATH>` names a different env file, resolved inside the folder when relative; the folder of the env file in use stays the agent folder for `control/` and `usdcx/`, so leave `--env-file` out to keep all of an agent's state in one place.

Without `--config-dir`, `--env-file <PATH>` loads a specific env file instead of the default `.env` lookup, and an explicit `--env-file` overrides variables already set in the shell. Without either flag, `.env` is searched for in the current directory and its parents, and already-set shell variables win. Agents started from the same working directory share its `agent-state.json`, `backups/` and `logs/`, even with one `--env-file` each, so give each agent its own folder and start it with `--config-dir`.

**The agent folder.** The directory of the env file in use (the current directory when there is none) is the agent's folder, and the agent keeps its local state there:

| Path | Contents |
|---|---|
| `.env` | the key, the settings and the control marker `AGENT_CONTROL_HEAD` |
| `control/` | the control store of a linked agent: its log, head, pending entry, policies, evidence, commands, counters and locks (see [docs/control.md](docs/control.md#where-the-state-lives)) |
| `usdcx/<network>/<party>/` | the `usdcx` burn and deposit logs |

Give each agent its own folder, and back up, copy or move the **whole folder**: the marker in `.env` and the store in `control/` must stay together, or a linked agent refuses to start. `cloud-agent control show` prints the store's path. `SILVANA_CONTROL_DIR` (or `control --control-dir`) names another folder to hold `control/`. Earlier versions kept this state in `~/.silvana-agent`; the agent moves its own part of it into the agent folder once, checks the copy, and leaves the old folder unchanged (see [docs/control.md](docs/control.md#moving-from-silvana-agent)). Agents that shared one folder with one env file each (`agent1.env`, `agent2.env`) need a folder each before their first start with this version.

#### Identity flags

The agent identity can also be passed on the command line, **before** the subcommand:

```bash
cloud-agent info balance --party <ID> --private-key <B58>
cloud-agent --party <ID> --private-key <B58> info balance   # equivalent
cloud-agent --party <ID> agent            # prompts for the key in a terminal
cloud-agent --quote-private-key <HEX> agent
```

`--party` overrides `PARTY_AGENT`, `--private-key` overrides `PARTY_AGENT_PRIVATE_KEY`, and `--quote-private-key` overrides `ATOMIC_QUOTE_PRIVATE_KEY`. Precedence is flag, then environment (`.env`/shell), then — for the private key only — a hidden terminal prompt when a party is known but no key is configured. The flags may appear before or after the subcommand. Without a terminal, or when no key is entered within 5 minutes, the command exits with an error, so non-interactive runs need the variable or the flag. `info network`, `info party`, `atomic keygen` and `sign` with an explicit `--private-key` need no agent key at all. The quote key is never prompted for. Keys supplied this way are not written to `.env`.

#### `.env` — Environment Variables

| Variable                         | Description                                                            | Written by `onboard` |
| -------------------------------- | ---------------------------------------------------------------------- | :------------------: |
| `PARTY_AGENT`                    | Your Canton party ID                                                   |         yes          |
| `PARTY_AGENT_PRIVATE_KEY`        | Base58 Ed25519 private key (32-byte seed); alternatively `--private-key` or a terminal prompt | generated keys only |
| `PARTY_AGENT_PUBLIC_KEY`         | Base58 Ed25519 public key (derived from the private key)               |         yes          |
| `ORDERBOOK_GRPC_URL`             | Orderbook gRPC endpoint                                                |         yes          |
| `CANTON_CHAIN`                   | `devnet` \| `testnet` \| `mainnet`                                     |         yes          |
| `SYNCHRONIZER_ID`                | Canton synchronizer ID                                                 |         yes          |
| `NODE_NAME`                      | Canton node name for routing                                           |         yes          |
| `VENUE_BRANCH`                   | RFQ V2 analytics branch stamped into this agent's JWT; slug `^[a-z0-9][a-z0-9-]{1,19}$`. Unset = server default `main`. Production cloud-agents: `agent` |          no          |
| `RFQ_V2_ONLY`                    | Overrides the top-level `rfq_v2_only` flag in `agent.toml` (same idiom as `RFQ_V2_ENABLED`: inert without a `[liquidity_provider]` section) — see the `agent.toml` section below |          no          |
| `LEDGER_SERVICE_PUBLIC_KEY`      | Base58 public key of the ledger service (for verifying responses)      |         yes          |
| `DSO`                            | DSO (Canton Coin admin) party ID                                       |         yes          |
| `PARTY_SETTLEMENT_OPERATOR`      | Settlement operator party ID                                           |         yes          |
| `PARTY_ORDERBOOK_FEE`            | Orderbook fee collection party                                         |         yes          |
| `PARTY_FEE`                      | Platform fee party: once linked, the agent pays fees, a swap quote's fee included, only to this party, and refuses them while it is unset (see [docs/control.md](docs/control.md#platform-parties)) | once, when unset |
| `PARTY_PREPAID_TRAFFIC`          | Prepaid traffic party: once linked, the agent tops up prepaid traffic only to this party, and refuses top-ups while it is unset | once, when unset |
| `AGENT_FEE_RESERVE_CC`           | CC balance held back for fees (default: `5.0`; a finite number)        |          no          |
| `AGENT_SETTLEMENT_TRAFFIC_CC`    | Prepaid CC held per in-flight settlement for its own transactions, apart from its fees (default: `1.2`; a finite number, at least 0) | no |
| `MERGE_THRESHOLD`                | Merge worker triggers when selectable amulets exceed this count        |         yes          |
| `MERGE_MAX_AMULETS`              | Max amulets merged per round (default: `100`)                          |         yes          |
| `MERGE_POLL_INTERVAL_SEC`        | Merge worker poll interval in seconds (default: `600`; at least 1)     |         yes          |
| `LOG_DESTINATION`                | `console` \| `file` (paired with `LOG_DIR`, `LOG_FILE_PREFIX`)         |         yes          |
| `RECURRING_PAYMENT_PACKAGE_NAME` | Recurring-payment package name; required for `subscription *` commands |         yes          |
| `SETTLEMENT_THREAD_COUNT`        | Concurrent settlement threads, 1–1024 (default `25` when unset)        |          no          |
| `MAX_ALLOCATION_WORKERS`         | Concurrent allocation payments, 1–256 (default `20` when unset)        |          no          |
| `MAX_FEE_WORKERS`                | Concurrent fee payments, 1–256 (default `5` when unset)                |          no          |
| `MAX_PAYMENT_WORKERS`            | Sets both payment pools at once, 1–256; overrides the two above        |          no          |
| `MIN_PREPAID_TRAFFIC_BALANCE_CC` | Auto top-up floor for the prepaid-traffic balance that settlement fees are paid from; the agent also tops up when the available prepaid balance (balance plus credit) is below what in-flight settlements still hold in prepaid (their unpaid fees and traffic holds). Set together with `PREPAID_TRAFFIC_TOPUP_CC` | yes |
| `PREPAID_TRAFFIC_TOPUP_CC`       | CC moved into the prepaid-traffic balance per top-up                   |         yes          |
| `MAX_SETTLEMENT_RETRIES`         | Failed attempts of one settlement step before the agent abandons the settlement and, if it has not allocated yet, cancels it; permanent errors abandon at once (default `30`, about 28 min of backoff) | no |
| `AGENT_MAX_PENDING_PER_COUNTERPARTY` | Open settlements allowed with one counterparty; further proposals are rejected (default `10`) | no |

#### Sequencer congestion (advanced)

None of these are written by `onboard`; the defaults are what production runs. They tune how the agent yields sequencer capacity when the Global Synchronizer is busy. Work is shed lowest priority first: background housekeeping (the DvpProposal GC) yields first and resumes last, and RFQ quoting is throttled last — spreads widen below `SEQUENCER_OVERLOAD_THRESHOLD` and new RFQs are rejected 0.1 below it — so that settlements already in flight keep their slots. In-flight settlement steps, including the off-chain processing fees, are never paused.

| Variable                            | Default | Description                                                                                                       |
| ----------------------------------- | :-----: | ----------------------------------------------------------------------------------------------------------------- |
| `BACKGROUND_PAUSE_SECS`             |  `60`   | How long deadline-free background work (DvpProposal GC) pauses after a `SEQUENCER_BACKPRESSURE` reply. It is the last thing that should resume competing for slots |
| `SEQUENCER_OVERLOAD_THRESHOLD`      |  `0.5`  | Predicted issuance coefficient below which RFQ spreads widen 3x. RFQs are rejected 0.1 below this                  |
| `FORECAST_POLL_SECS`                |  `30`   | Issuance-forecast poll interval, 1–3600; any other value stops startup. A dead poller freezes every gate above, so this doubles as its liveness signal |
| `LEDGER_UNHEALTHY_FAILURE_THRESHOLD`|   `3`   | Consecutive sequencer-unreachable failures that dark quoting. Backpressure is deliberately excluded — the ledger is up, just congested |
| `LEDGER_UNHEALTHY_COOLDOWN_SECS`    |  `60`   | How long quoting stays dark after the breaker trips, 0–86400; any other value stops startup                         |
| `MAX_RETRIES`                       |   `5`   | Retries per transaction submission, at least 1; `0` or a non-number stops startup. No new attempt starts after 120s and the wait between attempts is capped at 30s. Background operations ignore this and give up on the first backpressure reply |

#### DvpProposal GC (advanced)

Archives expired legacy-DVP proposals from the party's ACS. Also unwritten by `onboard`. A timer or coefficient value that does not parse or is out of range stops startup. The two switches take `1`, `true`, `yes`, `on` or `0`, `false`, `no`, `off`; a blank value uses the default, and any other value uses the default with a warning.

| Variable                     | Default | Description                                                                                    |
| ---------------------------- | :-----: | ---------------------------------------------------------------------------------------------- |
| `DVP_GC_ENABLED`             | `true`  | Master switch                                                                                   |
| `DVP_GC_REFRESH_SECS`        | `3600`  | Interval between full ACS scans; at least 1                                                     |
| `DVP_GC_DELAY_SECS`          |   `2`   | Delay between archival transactions, and the gate's poll period; at least 1                     |
| `DVP_GC_SAFETY_MARGIN_SECS`  | `3600`  | Only proposals whose `settleBefore` is at least this far in the past are touched                |
| `DVP_GC_REJECT_ENABLED`      | `true`  | Also reject expired proposals where this party is the counterparty                              |
| `DVP_GC_MIN_COEFFICIENT`     | `0.68`  | Archival pauses while the predicted coefficient is below this; must be a finite number          |
| `DVP_GC_MAX_PAUSE_SECS`      |  `900`  | Abandon the cycle and rescan after the gate has been shut this long; at least 60 (raise it to effectively disable) |
| `DVP_GC_STALE_FORECAST_SECS` |  `300`  | Warn when the gate is deciding on a coefficient older than this                                 |

#### `agent.toml` — Agent Settings

Full example for an LP with grid orders and RFQ:

```toml
role = "trader"
auto_settle = true
poll_interval_secs = 7
token_ttl_secs = 3600
connection_timeout_secs = 30
# RFQ V2 (AtomicDVP) only — see below. Default: false.
rfq_v2_only = false

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
allocate_before_secs = 900
settle_before_secs = 1800
```

**`rfq_v2_only`** (top-level, default `false`, env override `RFQ_V2_ONLY`) forces the agent to quote via RFQ V2 (AtomicDVP) exclusively:

- The V1 LP settlement stream is never opened — no V1 LP registration, no V1 quotes, and the agent disappears from `GetConnectedLiquidityProviders`.
- No grid/limit orders are placed; `[[markets.bid_levels]]`/`[[markets.offer_levels]]` are ignored (keep them in the file for easy rollback). The startup cancel-all still runs, so flipping the switch on and restarting clears any resting orders.
- The mid-price poller, balance refresh, and the settlement subscription keep running. **V1 transition behavior:** an in-flight V1 DVP that still needs *this agent's* fee or allocation steps is **actively cancelled server-side on encounter** — reservations released and the counterparty notified within one poll cycle, instead of stranding until deadline expiry (without live V1-stream registration the server's fee gate would classify the agent as a regular user and the DVP could never complete). One this agent has already allocated for is left alone and can still settle via the operator. Leftover on-chain DvpProposals are reaped by the DvpProposal GC; the counterparty's already-paid fees are not refunded — still prefer flipping during a quiet V1 window.
- Startup fails loudly unless `[liquidity_provider.rfq_v2].enabled = true` and at least one enabled market has `[markets.rfq.v2].enabled = true`; a failed RFQ V2 setup is also fatal in this mode (nothing left to quote). `--orders-only` conflicts with it.

**`[control]` and `[vetted_packages]`** apply to an agent linked to controllers (see [docs/control.md](docs/control.md)); `[vetted_packages]` applies to every agent. Both are optional. They are read strictly: an unknown key, a value out of range, or one of their keys placed at the top level of agent.toml or below another table's header stops startup with a message naming the section and the key. The running agent logs both at startup and reads them only then, so restart it after a change; each `control` command reads them when it runs.

```toml
[control]
auto_execute_commands = false   # run pending direct commands without a prompt (default false)
auto_accept_policies = true     # accept valid pending policies without a prompt (default true)
swap_max_deviation_bps = 100    # largest gap between a swap's price and the reference mid; 0 turns it off; at most 10000
swap_reference_max_age_secs = 300 # oldest reference mid the guard takes, in seconds; 30 to 3600

[vetted_packages]
mode = "selected"               # or "all", with no packages key (the default)
packages = [
  "<64 lowercase hex package id>", # name version
]
```

`selected` takes 1 to 256 different package ids. The Agent page's Packages tab copies this section for you. With `selected`, the agent checks every transaction before it signs it and refuses one that uses a package outside the list (`package_not_allowed`), and it signs only when its own hash of the transaction matches the server's, so the bytes it checked are the bytes it signs; `all` checks nothing. A linked agent also checks the packages rule of its signed policy before every signature, and the ledger service can refuse a transaction that uses a package outside it.

An optional `[verify]` section sets `mode = "default"`, `"observe"` or `"enforce"`: how the verifier's newer checks act while the agent is not linked (see [docs/control.md](docs/control.md)). A linked agent enforces them all.

### Grid Orders

When `agent` runs, it places limit orders on a grid of price levels around the current mid price.

Each level is defined by:

- **`delta_percent`** — signed offset from mid price in percent. Bids use negative values (below mid), offers use positive values (above mid). E.g. `-0.5` on a bid = 0.5% below mid; `0.3` on an offer = 0.3% above mid.
- **`quantity`** — order size at this level

Each side (bids, offers) is re-placed on its own when the mid moves by `price_change_threshold_percent` or more from the price that side was last placed at, when one of its rungs is missing, extra or partially filled, or when re-placing only one side would cross the other side's resting orders (both sides are then re-placed). After a restart, or once a lost price feed recovers, both sides are re-placed. A side the current unlocked balance cannot fund has its resting orders withdrawn until it can be funded again. A market with neither side fundable and nothing resting is re-checked every 60s.

### RFQ (Request for Quote)

When `[liquidity_provider]` is configured (and `rfq_v2_only` is off), the agent connects to the orderbook server via a bidirectional gRPC stream (`SettlementStream`) and receives RFQ requests in real time.

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

- **`allocate_before_secs`** — seconds from DVP creation to allocate tokens (default: 900)
- **`settle_before_secs`** — seconds from DVP creation to complete settlement (default: 1800)

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
| `--interval <SECS>`    | Retry interval when no quote arrives, at least 1 (default: 60) |

Amounts and the price limit must be finite numbers above 0. When a round's settle may have committed but this cannot be confirmed, the loop stops with an error instead of requesting a new quote; check the ledger before you restart it.

Progress is saved to `agent-state.json` in the working directory, and each round is marked there before it can commit. If that file cannot be read or written, or belongs to another party, the loop does not start, or stops before the round is submitted. A restart reads the settlement status of a marked V1 round up to six times, 10s apart, before quoting; a V1 round whose outcome is still unknown stops the restart until a rerun can read its status, and a marked atomic round stops it with the order's progress and the edits to `agent-state.json` that continue it. With `--confirm`, a prompt left unanswered stops the loop instead of accepting another quote.

### `agent` mode — flags

| Flag                | Description                                                  |
| ------------------- | ------------------------------------------------------------ |
| `--settlement-only` | Disable order placement, only settle existing trades         |
| `--orders-only`     | Disable settlement, only place/manage grid orders (conflicts with `rfq_v2_only = true`) |
| `--no-restore`      | Skip loading previous state from `agent-state.json`          |
| `--no-reject`       | Accept all proposals without RFQ-state verification          |
| `--dry-run`         | Prepare and verify transactions without signing or executing (not available for `agent`; `buy` and `sell` still accept real quotes) |
| `--force`           | Sign and execute even if verification fails (never under a `[vetted_packages]` selection) |
| `--confirm`         | Prompt for confirmation before signing each transaction; no answer within 120s, or 120s waiting behind another prompt, declines it |
| `--verbose`         | Enable verbose logging                                       |
| `--config <PATH>`   | Path to agent.toml (default: `agent.toml`)                   |
| `--config-dir <DIR>` | Run from this agent folder: `.env`, `agent.toml`, `agent-state.json`, `backups/` and `logs/` live there |
| `--shutdown-timeout-secs <SECS>` | Longest stop after Ctrl-C, 1–3600 seconds (default: no overall limit) |

**Stopping the agent.** The first Ctrl-C starts a graceful stop: the agent takes no new work, lets the settlement steps in flight finish (up to 5 minutes for settlement tasks; a main-loop step can take up to `canton_op_timeout_secs`, default 600 s), cancels its grid orders and saves `agent-state.json`, which the next start resumes from. With `--shutdown-timeout-secs N` the stop is bounded: after N seconds the steps still running are dropped and picked up again at the next start, cancelling grid orders still gets up to 30 s, and the state is always saved, so a stop takes at most about N + 70 seconds. A second Ctrl-C cuts the stop short, saves the state and exits within 45 seconds; a third exits at once without saving.

### Full CLI Reference

| Command                                                                                       | Description                                             |
| --------------------------------------------------------------------------------------------- | ------------------------------------------------------- |
| `agent`                                                                                       | Long-running agent (grid + settlement; RFQ as an LP)    |
| `onboard`                                                                                     | Self-service onboarding                                 |
| `generate-private-key`                                                                        | Generate a new Ed25519 keypair (no config needed)       |
| `<command> --party <ID> --private-key <B58>`                                                  | Identity from flags instead of `.env` (either position) |
| `--quote-private-key <HEX> <command>`                                                         | Quote key from a flag instead of `.env`                 |
| `info balance`                                                                                | Show token balances                                     |
| `info party`                                                                                  | Show party ID, public key, node name                    |
| `info network`                                                                                | Show DSO party, rates, mining rounds                    |
| `info prepaid-traffic`                                                                        | Show the prepaid-traffic balance and credit limit       |
| `info list [--template ID] [--count]`                                                         | List active contracts                                   |
| `buy --market <ID> --amount <N>`                                                              | Buy via RFQ until filled                                |
| `sell --market <ID> --amount <N>`                                                             | Sell via RFQ until filled                               |
| `faucet get --token <CC\|USDC\|…> [--admin <P>] [--amount <N>]`                               | Request tokens (devnet)                                 |
| `transfer send-cc --receiver <P> --amount <N\|all>`                                           | Send Canton Coin                                        |
| `transfer send-cip56 --receiver <P> --instrument-id <ID> [--instrument-admin <P>] --amount <N\|all> [--count <N>]` | Send CIP-56 token                |
| `transfer accept-cip56 --contract-id <CID>`                                                   | Accept incoming CIP-56 transfer                         |
| `transfer split-cc --output-amounts a,b,c --amulet-cids x,y`                                  | Split CC amulets                                        |
| `transfer batch-pay --file payments.csv`                                                      | Batch CC payments (atomic multicall)                    |
| `transfer prepay-traffic --amount <N\|all>`                                                   | Top up the off-chain prepaid traffic balance            |
| `preapproval request --instrument-admin <P>`                                                  | Create a `TransferPreapproval`                          |
| `preapproval fetch`                                                                           | List existing preapprovals                              |
| `preapproval check`                                                                           | Compare held preapprovals against the advertised targets |
| `preapproval sync`                                                                            | Create any advertised preapproval this party lacks      |
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
| `usdcx status` / `usdcx onboard` / `usdcx deposit` / `usdcx mint` / `usdcx burn` / `usdcx track` | USDCx bridge (see below)                             |
| `control link --code <LNK-…>` / `control add` / `control remove` / `control capabilities`     | Link the agent with a link code, and change its controllers. The Agent page makes no link code for an existing agent yet: a new agent links during `onboard` with an `AGT-<invite>-<link>` code |
| `control show [--offline] [--json]`                                                           | Controllers, pending changes, policy, log head, marker  |
| `control policy show [--pending]` / `control policy accept [--hash <H>]`                      | Show the policy; accept a pending one                   |
| `control commands` / `control execute --code CMD-<id>`                                        | List the pending direct commands; run one               |
| `control sync` / `control restore` / `control discard-pending`                                | Keep the local control log and the rpc's copy in step   |
| `controller keygen --dir <DIR>` / `controller sign --key <FILE> --network <NET> …`            | Ed25519 controller key: create it, sign a document (offline) |
| `controller status` / `log` / `holdings` / `traffic` / `activity` / `ledger` / `packages` / `commands` | Read a linked agent with an Ed25519 controller key |
| `controller policy show [--rules]` / `controller policy set …`                                | Show or sign the agent's policy                         |
| `controller command transfer-cc` / `transfer-token` / `swap` / `show --code CMD-<id>`         | Sign a direct command, or follow one to its receipt     |

The `control` and `controller` commands are described in [docs/control.md](docs/control.md).

### USDCx bridge (Ethereum USDC ⇄ Canton USDCx)

The `usdcx` commands move USDC between Ethereum and Canton through Circle's xReserve and Digital Asset's USDCx bridge (MainNet and TestNet). Canton operations are typed ledger-service operations: the server builds the transaction, the agent verifies it strictly (pinned Digital Asset parties and bridge package, matching hash; `--force` is refused) and signs it with the party's key.

| Command | Description |
| --- | --- |
| `usdcx status` | Agreement, pending deposits and withdrawals, USDCx balance, where deposits go |
| `usdcx onboard [--pre-approval] [--request-id <UUID>]` | Request the bridge agreement and wait for Digital Asset (about a minute) |
| `usdcx deposit --amount <N> --print-only [--from <0x…>]` | Print Digital Asset's deposit page and the two transactions to send from your own wallet |
| `usdcx deposit --amount <N> [--no-mint]` | Send the deposit from here: the Ethereum key is typed at a hidden prompt and never stored (needs `ETHEREUM_RPC_URL`); then mint it on Canton |
| `usdcx mint [--attestation-cid <CID>] [--wait]` | Mint a deposit's `DepositAttestation` (not needed with pre-approval: Digital Asset mints directly) |
| `usdcx burn --amount <N> --to <0x…> [--request-id <UUID>] [--fee-on-top]` | Burn USDCx; Circle releases the USDC on Ethereum minus its fee |
| `usdcx reclaim --revoked-cid <CID>` | Take back the USDCx of a withdrawal Digital Asset revoked |
| `usdcx pre-approval --enable\|--disable` | Switch direct minting of deposits |
| `usdcx offboard [--reason <TEXT>]` | End the bridge agreement |
| `usdcx track [--eth-tx <0x…>] [--wait]` | Continue open withdrawals and deposits; `--eth-tx` registers a deposit sent from a wallet |

Options: `--network mainnet|testnet` (default: the network of `SYNCHRONIZER_ID`), `--state-dir <DIR>` (default `<agent folder>/usdcx`), `--yes` (no confirmations), and the global `--dry-run` (prepare and verify only).

- **Never send a plain USDC transfer to the xReserve address**: it carries no Canton recipient and does not reach Canton. Deposit only on the network's Ethereum chain (chain id 1 on MainNet).
- **Withdrawals are guarded locally.** `burn` prints its request id before anything is sent and records every signature in `<state-dir>/<network>/<party>/burns.json` before making it. If a run is interrupted or its outcome is unclear, continue with `usdcx track` or `usdcx burn --request-id <the printed id>` with the same amount and destination — never by starting a new withdrawal: a new one is refused while an earlier one may still commit.
- **Exit codes**: 0 done; 1 nothing was committed (the command may be run again); 3 something was or may have been submitted and is not finished (continue as above).
- Every operation pays the ledger service's traffic fee; top up with `transfer prepay-traffic` first.

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

Both LP streams (this one and the atomic RFQ stream) reconnect after 5s when the stream ends, the open gets no response within 30s, an outbound send stalls for 10s, or the server goes silent for 90s after its first heartbeat.

The agent opens this stream when `[liquidity_provider]` is configured in `agent.toml` — unless `rfq_v2_only = true`, in which case the stream is never opened and the agent never appears in `GetConnectedLiquidityProviders`.

#### Authentication

- **JWT** — Self-describing Ed25519 JWT (RFC 8037) with automatic refresh. Used for Orderbook, Pricing, and Settlement RPCs.
- **Message signing** — Per-request Ed25519 signature on the canonical request payload, used for `DAppProviderService` two-phase transactions. Server responses are also signed and the agent verifies them using `LEDGER_SERVICE_PUBLIC_KEY`.

### Security Model

- **Two-phase signing** — The server prepares a transaction and returns the bytes and a hash. The agent independently recomputes the hash, verifies the transaction with `tx-verifier`, and only then signs.
- **Transaction verification** — Before signing, `tx-verifier` checks that the prepared transaction matches the requested operation (correct template, parties, amounts). This prevents the server from tricking the agent into signing unexpected transactions.
- **Ed25519 JWT authentication** — Self-describing JWTs embed the Ed25519 public key (RFC 8037); the server verifies the public key fingerprint matches the party ID.
- **Private key isolation** — The Ed25519 private key is only used locally for signing transaction hashes and JWTs. It is never transmitted to the server.
