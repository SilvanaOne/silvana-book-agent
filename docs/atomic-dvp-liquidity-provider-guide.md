# Atomic DVP Liquidity Provider — Setup Guide (devnet & mainnet)

This guide takes you, step by step, from nothing to a **running Atomic DVP liquidity provider (LP)**.
You do **not** need to know how to code — you just download a ready‑made program and run a few
commands.

A liquidity provider answers price requests ("quotes") from traders and completes each swap in a
single transaction. This guide sets up **only** the Atomic DVP swap flow — it does **not** place
order‑book orders, and it does **not** use the older RFQ V1 flow.

> **The binary is `cloud-agent`.** Some example configs carry an old
> `cargo run -p orderbook-cloud-agent …` comment — ignore it; the tool you run is `cloud-agent`.
>
> **There is no `atomic start` command.** The provisioning commands are `atomic setup` and
> `atomic status`; you then **start the long‑running agent with `cloud-agent agent`**.

## What you need before you start

- A machine: **macOS (Apple Silicon)**, **Linux x86_64**, or **Linux ARM64**.
- An **invite code** to onboard. For **devnet**, use the public code `INVITE1`. For **mainnet**,
  request a code from Silvana.
- Funds to quote with:
  - **devnet** — free, auto‑requested from the faucet during onboarding.
  - **mainnet** — you must fund the party yourself (no faucet).
- One working directory that will hold your `.env` and `agent.toml`. **Run every command from
  that directory** — the agent auto‑loads `.env` and `agent.toml` from the current directory.

## The two keys (important)

An Atomic DVP LP uses **two different keys**. Keep both secret; neither ever leaves your machine.

| Key | Env var | What it is | How you get it |
| --- | --- | --- | --- |
| **Party key** | `PARTY_AGENT_PRIVATE_KEY` | Ed25519 — your Canton party identity | **Created automatically** by `cloud-agent onboard` |
| **Quote key** | `ATOMIC_QUOTE_PRIVATE_KEY` | secp256k1 — signs every Atomic DVP quote | **You generate it** with `cloud-agent atomic keygen` and paste it into `.env` |

The quote key is **only** needed for Atomic DVP. If `[liquidity_provider.rfq_v2].enabled = true` but
`ATOMIC_QUOTE_PRIVATE_KEY` is missing from `.env`, the agent refuses to start.

---

# PART A — DEVNET

Work inside a fresh directory, e.g. `mkdir lp-devnet && cd lp-devnet`.

## A1. Download the `cloud-agent` program

Find the block for your computer below and copy‑paste all four lines into a terminal.

**macOS (Apple Silicon):**

```bash
curl -LO https://github.com/SilvanaOne/silvana-book-agent/releases/latest/download/cloud-agent-macos-silicon.tar.gz
tar -xzf cloud-agent-macos-silicon.tar.gz
chmod +x cloud-agent
./cloud-agent --help
```

**Linux (x86_64):**

```bash
curl -LO https://github.com/SilvanaOne/silvana-book-agent/releases/latest/download/cloud-agent-x86_64-linux.tar.gz
tar -xzf cloud-agent-x86_64-linux.tar.gz
chmod +x cloud-agent
./cloud-agent --help
```

**Linux (ARM64):**

```bash
curl -LO https://github.com/SilvanaOne/silvana-book-agent/releases/latest/download/cloud-agent-arm64-linux.tar.gz
tar -xzf cloud-agent-arm64-linux.tar.gz
chmod +x cloud-agent
./cloud-agent --help
```

If the last line prints a help message, you're set. You now have a `cloud-agent` program in the
current folder, and you run it by typing `./cloud-agent`.

## A2. Onboard — create your account and private key

Run one command (use your own name and email). devnet is the default and its invite code is the
public `INVITE1`, so the command below works as‑is. It's safe to re‑run if anything goes wrong.

```bash
./cloud-agent onboard --agent-name my-lp --email me@example.com --invite-code INVITE1
```

This can take a minute or two — it waits for the network to set up your account (you'll see it
poll every few seconds). That's normal.

When it finishes it creates two files in the current folder for you: **`.env`** (your identity,
private key, and network settings) and a starter **`agent.toml`** (you'll replace it in Step A5).
Your private key stays on your machine.

Optional — confirm it worked: `./cloud-agent info party`

## A3. Generate the Atomic DVP quote key

`atomic keygen` prints a new secp256k1 key line to paste into `.env`. It never writes a file.

```bash
./cloud-agent atomic keygen
```

Copy the printed line into your `.env`:

```dotenv
ATOMIC_QUOTE_PRIVATE_KEY=<64-hex-character scalar printed by atomic keygen>
```

Re‑running `atomic keygen` once the var is set just prints `ATOMIC_QUOTE_PRIVATE_KEY is set and
valid.` plus the public key — a handy way to confirm `.env` is loaded.

## A4. Check your balance (devnet)

On devnet, onboarding already gave you a small amount of each test token (CC, USDC, EDELx, cETH,
CBTC). Just check:

```bash
./cloud-agent info balance
```

Optional — get more free test tokens any time:

```bash
./cloud-agent faucet get --token CC
./cloud-agent faucet get --token USDC
```

The more tokens you hold, the more swaps you can quote at once. These starter amounts are modest,
so the large ladders in Step A5 will only partly fill on a fresh devnet account — that's fine (see
the note in Step A6). You can top up later.

## A5. Configure `agent.toml` (devnet, Atomic DVP only)

Open the `agent.toml` file that onboarding created, delete everything in it, and paste the config
below. It turns on Atomic DVP for 8 markets. You can delete any `[[markets]]` blocks you don't
want — just keep the token amounts (`denominations`) for the tokens your markets use.

```toml
# ── agent.toml — DEVNET, Atomic DVP only (no grid orders, no RFQ V1) ──────────────
# Markets: CC-USDC, CBTC-USDC, CC-CBTC, cETH-USDC, cETH-CC, EDELx-USDC, EDELx-CC, EDELx-cETH
# Run with: ./cloud-agent agent

auto_settle = true
poll_interval_secs = 7
role = "trader"
token_ttl_secs = 3600
connection_timeout_secs = 30

# Liquidity-provider identity (its presence enables RFQ handling)
[liquidity_provider]
name = "My LP"
# For Atomic DVP this is NOT a hard concurrency cap — concurrency is bounded by the
# number of ready ladder holdings (below). 120 aligns the status display with a
# ladder sized for ~100 concurrent $10-20 swaps + 20% headroom.
max_concurrent_rfqs = 120
default_quote_valid_secs = 60
# Refuse RFQs whose USD value is below this ($10).
min_notional_usd = 10

# Atomic DVP — one-transaction atomic settles with secp256k1-signed
# quotes. Requires ATOMIC_QUOTE_PRIVATE_KEY in .env.
[liquidity_provider.rfq_v2]
enabled = true
# Quotes at/above this USD notional consume a single-use SettlementTicket;
# smaller quotes are ticketless.
ticket_threshold_usd = 50

# GLOBAL per-instrument denomination ladders ("AMOUNTxCOUNT"), shared by every
# market that pays that instrument. Each in-flight quote hard-reserves ONE
# holding of the leg the LP pays, so effective concurrency = ready holdings per
# instrument. Devnet prices: CC ~$0.14, USDC $1, EDELx ~$0.0136, CBTC ~$80k,
# cETH ~$1760.
[liquidity_provider.rfq_v2.denominations]
CC    = ["150x120"]      # 150 CC   ~= $21
USDC  = ["20x120"]       # $20      — quote leg of most markets
cETH  = ["0.012x120"]    # ~= $21
EDELx = ["1500x120"]     # ~= $20
CBTC  = ["0.00025x120"]  # ~= $20

# ── Markets ───────────────────────────────────────────────────────────────────
# Each market needs [markets.rfq] (enabled + min/max quantity) AND
# [markets.rfq.v2] enabled = true. No bid_levels/offer_levels = no grid orders.

[[markets]]
market_id = "CC-USDC"
enabled = true
[markets.rfq]
enabled = true
min_quantity = "5"
max_quantity = "1000"
bid_spread_percent = 0.5
offer_spread_percent = 0.5
quote_valid_secs = 60
allocate_before_secs = 900
settle_before_secs = 1800
[markets.rfq.v2]
enabled = true

[[markets]]
market_id = "CBTC-USDC"
enabled = true
[markets.rfq]
enabled = true
min_quantity = "0.00001"
max_quantity = "0.01"
bid_spread_percent = 0.5
offer_spread_percent = 0.5
quote_valid_secs = 60
allocate_before_secs = 900
settle_before_secs = 1800
[markets.rfq.v2]
enabled = true

[[markets]]
market_id = "CC-CBTC"
enabled = true
[markets.rfq]
enabled = true
min_quantity = "5"
max_quantity = "1000"
bid_spread_percent = 0.5
offer_spread_percent = 0.5
quote_valid_secs = 60
allocate_before_secs = 900
settle_before_secs = 1800
[markets.rfq.v2]
enabled = true

[[markets]]
market_id = "cETH-USDC"
enabled = true
[markets.rfq]
enabled = true
min_quantity = "0.0001"
max_quantity = "0.1"
bid_spread_percent = 0.5
offer_spread_percent = 0.5
quote_valid_secs = 60
allocate_before_secs = 900
settle_before_secs = 1800
[markets.rfq.v2]
enabled = true

[[markets]]
market_id = "cETH-CC"
enabled = true
[markets.rfq]
enabled = true
min_quantity = "0.0001"
max_quantity = "0.1"
bid_spread_percent = 0.5
offer_spread_percent = 0.5
quote_valid_secs = 60
allocate_before_secs = 900
settle_before_secs = 1800
[markets.rfq.v2]
enabled = true

[[markets]]
market_id = "EDELx-USDC"
enabled = true
[markets.rfq]
enabled = true
min_quantity = "50"
max_quantity = "10000"
bid_spread_percent = 0.5
offer_spread_percent = 0.5
quote_valid_secs = 60
allocate_before_secs = 900
settle_before_secs = 1800
[markets.rfq.v2]
enabled = true

[[markets]]
market_id = "EDELx-CC"
enabled = true
[markets.rfq]
enabled = true
min_quantity = "50"
max_quantity = "10000"
bid_spread_percent = 0.5
offer_spread_percent = 0.5
quote_valid_secs = 60
allocate_before_secs = 900
settle_before_secs = 1800
[markets.rfq.v2]
enabled = true

[[markets]]
market_id = "EDELx-cETH"
enabled = true
[markets.rfq]
enabled = true
min_quantity = "50"
max_quantity = "10000"
bid_spread_percent = 0.5
offer_spread_percent = 0.5
quote_valid_secs = 60
allocate_before_secs = 900
settle_before_secs = 1800
[markets.rfq.v2]
enabled = true
```

**Trim it if you want fewer markets.** You only need the markets you intend to quote. Keep at
least one `[markets.rfq.v2] enabled = true` market, and keep a ladder entry for **each token that
appears** in your markets. See the [field reference](#agenttoml-field-reference) below.

## A6. Provision the venues — `atomic setup`

First, download the devnet service file into this folder — `atomic setup` needs it (it ships in
the repo at `examples/atomic-dvp/devnet/atomic-dvp-service.json`):

```bash
curl -LO https://raw.githubusercontent.com/SilvanaOne/silvana-book-agent/dev/examples/atomic-dvp/devnet/atomic-dvp-service.json
```

Then run the setup — this one command does all the on‑chain setup for you:

```bash
./cloud-agent atomic setup
```

It prints a 6‑step progress log and finishes with `Setup complete.`:

```
=== Atomic (RFQ V2) setup for <your-party> ===
[1/6] Quote key (from ATOMIC_QUOTE_PRIVATE_KEY): <public key>
[2/6] ...
[6/6] denomination splits …
Setup complete. Run `atomic status` to verify, then restart the agent.
```

You don't need to understand each step. A few things worth knowing:

- It's safe to **run this again any time** — for example after you fund a new token or add a
  market.
- For tokens you're light on, you'll see yellow `WARN` lines like `Split for cETH failed:
  insufficient holdings` or `partial split — balance … covers … of … ladder total`. **These are
  expected and safe** — setup still finishes with `Setup complete.` Those markets just start with
  fewer (or no) ready quotes until you fund those tokens and re‑run.

## A7. Verify — `atomic status`

This shows your current status and changes nothing. Run it:

```bash
./cloud-agent atomic status
```

Check that:

- Each of your markets is listed, and its quote key says **`MATCHES local keyfile`**. If it says
  **DOES NOT MATCH**, see [Troubleshooting](#troubleshooting).
- Your funded tokens show some entries under holdings.

## A8. Start the agent — `cloud-agent agent`

This starts your liquidity provider. Leave it running (press Ctrl‑C to stop).

```bash
./cloud-agent agent
```

You're live once you see this line in the output:

```
LP atomic stream connected, listening for atomic RFQ requests
```

Leave the program running to keep quoting. To keep it running after you close the terminal, use a
tool like `tmux` or run it as a service.

---

# PART B — MAINNET

Same eight steps, in a **separate directory** (e.g. `mkdir lp-mainnet && cd lp-mainnet`) with its
own `.env` and `agent.toml`. The differences from devnet are called out at each step.

## B1. Download the `cloud-agent` program

Exactly the same as [Step A1](#a1-download-the-cloud-agent-program) — the same program works on
every network. If you already downloaded it, you can reuse it (just work in this new folder).

## B2. Onboard on mainnet

Same command as devnet, plus `--rpc https://orderbook-mainnet.silvana.dev:443` at the end to pick
the mainnet network:

```bash
./cloud-agent onboard --agent-name my-lp --email me@example.com --invite-code YOUR_INVITE_CODE --rpc https://orderbook-mainnet.silvana.dev:443
```

As before, this creates your `.env` and starter `agent.toml`. Optional check:
`./cloud-agent info party`

## B3. Generate the Atomic DVP quote key

Same as [Step A3](#a3-generate-the-atomic-dvp-quote-key) — generate a **fresh** key for mainnet
and paste the printed line into this folder's `.env`:

```bash
./cloud-agent atomic keygen
```

## B4. Fund the party (mainnet — no faucet)

There is **no free faucet on mainnet**. Before you can quote, send real tokens to your account
(run `./cloud-agent info party` to see your account ID to send to). Fund every token used by your
markets. As a rough guide for a full‑size setup:

| Token | Suggested amount to hold |
| --- | --- |
| CC | ~18,000 |
| USDCx | ~2,400 |
| cETH | ~1.44 |
| CBTC | ~0.03 |
| EDELx | ~180,000 |

You don't need all of it — with less, you simply quote fewer swaps at once. Check what you hold:

```bash
./cloud-agent info balance
```

## B5. Configure `agent.toml` (mainnet, Atomic DVP only)

Same shape as devnet, but the **quote token is `USDCx`** and the market IDs differ. This mirrors
the production mainnet LP config, with grid orders removed.

```toml
# ── agent.toml — MAINNET, Atomic DVP only (no grid orders, no RFQ V1) ─────────────
# Markets: CC-USDCx, CBTC-USDCx, CBTC-CC, cETH-USDCx, cETH-CC, EDELx-USDCx, EDELx-CC, EDELx-cETH
# Run with: ./cloud-agent agent

auto_settle = true
poll_interval_secs = 7
role = "trader"
token_ttl_secs = 3600
connection_timeout_secs = 30

[liquidity_provider]
name = "My LP"
max_concurrent_rfqs = 120
default_quote_valid_secs = 60
min_notional_usd = 10

[liquidity_provider.rfq_v2]
enabled = true
ticket_threshold_usd = 50

# GLOBAL per-instrument ladders. Mainnet prices: CC ~$0.134, USDCx $1,
# EDELx ~$0.0136, CBTC ~$80k, cETH ~$1760.
[liquidity_provider.rfq_v2.denominations]
CC    = ["150x120"]      # 150 CC ~= $20
USDCx = ["20x120"]       # $20 — quote leg of most markets
cETH  = ["0.012x120"]    # ~= $21
EDELx = ["1500x120"]     # ~= $20
CBTC  = ["0.00025x120"]  # ~= $20

# ── Markets ───────────────────────────────────────────────────────────────────

[[markets]]
market_id = "CC-USDCx"
enabled = true
[markets.rfq]
enabled = true
min_quantity = "5"
max_quantity = "1000"
bid_spread_percent = 1
offer_spread_percent = 1
quote_valid_secs = 60
allocate_before_secs = 900
settle_before_secs = 1800
[markets.rfq.v2]
enabled = true

[[markets]]
market_id = "CBTC-USDCx"
enabled = true
[markets.rfq]
enabled = true
min_quantity = "0.00001"
max_quantity = "0.01"
bid_spread_percent = 0.5
offer_spread_percent = 0.5
quote_valid_secs = 60
allocate_before_secs = 900
settle_before_secs = 1800
[markets.rfq.v2]
enabled = true

[[markets]]
market_id = "CBTC-CC"
enabled = true
[markets.rfq]
enabled = true
min_quantity = "0.00001"
max_quantity = "0.01"
bid_spread_percent = 0.5
offer_spread_percent = 0.5
quote_valid_secs = 60
allocate_before_secs = 900
settle_before_secs = 1800
[markets.rfq.v2]
enabled = true

[[markets]]
market_id = "cETH-USDCx"
enabled = true
[markets.rfq]
enabled = true
min_quantity = "0.0001"
max_quantity = "0.1"
bid_spread_percent = 0.5
offer_spread_percent = 0.5
quote_valid_secs = 60
allocate_before_secs = 900
settle_before_secs = 1800
[markets.rfq.v2]
enabled = true

[[markets]]
market_id = "cETH-CC"
enabled = true
[markets.rfq]
enabled = true
min_quantity = "0.0001"
max_quantity = "0.1"
bid_spread_percent = 0.5
offer_spread_percent = 0.5
quote_valid_secs = 60
allocate_before_secs = 900
settle_before_secs = 1800
[markets.rfq.v2]
enabled = true

[[markets]]
market_id = "EDELx-USDCx"
enabled = true
[markets.rfq]
enabled = true
min_quantity = "50"
max_quantity = "10000"
bid_spread_percent = 2.5
offer_spread_percent = 0.0
quote_valid_secs = 60
allocate_before_secs = 900
settle_before_secs = 1800
[markets.rfq.v2]
enabled = true

[[markets]]
market_id = "EDELx-CC"
enabled = true
[markets.rfq]
enabled = true
min_quantity = "50"
max_quantity = "10000"
bid_spread_percent = 3.0
offer_spread_percent = 0.0
quote_valid_secs = 60
allocate_before_secs = 900
settle_before_secs = 1800
[markets.rfq.v2]
enabled = true

[[markets]]
market_id = "EDELx-cETH"
enabled = true
[markets.rfq]
enabled = true
min_quantity = "50"
max_quantity = "10000"
bid_spread_percent = 2.5
offer_spread_percent = -2.4
# Keep the firm spread under ALL conditions: never widen for sequencer load or
# one-sided liquidity depletion.
disable_overload_spread_widening = true
disable_depletion_spread_widening = true
quote_valid_secs = 60
allocate_before_secs = 900
settle_before_secs = 1800
[markets.rfq.v2]
enabled = true
```

## B6. Provision the venues — `atomic setup`

Same as [Step A6](#a6-provision-the-venues--atomic-setup), but use the **mainnet** service file:
request `atomic-dvp-service.json` for mainnet from Silvana and place it in this folder first (the
repo only ships the devnet one).

```bash
./cloud-agent atomic setup
```

It only sets up the tokens you actually funded in Step B4, so fund first, then run this.

## B7. Verify — `atomic status`

Identical to [A7](#a7-verify--atomic-status):

```bash
./cloud-agent atomic status
```

Confirm each mainnet market has a venue whose `quote key: MATCHES local keyfile`, and that your
funded tokens show ladder rungs.

## B8. Start the agent — `cloud-agent agent`

```bash
./cloud-agent agent
```

You're live once you see `LP atomic stream connected, listening for atomic RFQ requests`. Leave it
running (use `tmux` or run it as a service to keep it up after closing the terminal).

---

# `agent.toml` field reference

Only four keys have **no default** and must be present: `liquidity_provider.name`, each
`markets[].market_id`, and each market's `markets.rfq.min_quantity` / `max_quantity`. Everything
else falls back to the defaults shown.

### Top level

| Key | Default | Meaning |
| --- | --- | --- |
| `auto_settle` | `true` | Settle accepted trades automatically |
| `poll_interval_secs` | `5` | Main loop poll interval |
| `role` | `"trader"` | Agent role |
| `token_ttl_secs` | `3600` | Auth token lifetime |
| `connection_timeout_secs` | `30` | gRPC connect timeout |

### `[liquidity_provider]`

| Key | Default | Meaning |
| --- | --- | --- |
| `name` | — (**required**) | Display name used in the RFQ stream handshake |
| `max_concurrent_rfqs` | `10` | For Atomic DVP, aligns the status display; **not** a hard cap (inventory bounds concurrency) |
| `default_quote_valid_secs` | `30` | Fallback quote validity |
| `min_notional_usd` | `0` (off) | Reject RFQs below this USD value |

### `[liquidity_provider.rfq_v2]` (the Atomic DVP master switch)

| Key | Default | Meaning |
| --- | --- | --- |
| `enabled` | `false` | **Set `true` to turn on Atomic DVP** (requires `ATOMIC_QUOTE_PRIVATE_KEY`) |
| `ticket_threshold_usd` | none | At/above this USD notional a quote consumes a single‑use ticket; unset = always ticketless |
| `atomic_quote_valid_secs` | `120` | Signed‑quote validity window |
| `settle_grace_secs` | `30` | Reservation TTL beyond the signed validity |
| `ticket_batch_size` | `50` | Tickets issued per batch |
| `ticket_low_water` | `50` | Re‑issue tickets when live count drops below this |

### `[liquidity_provider.rfq_v2.denominations]`

Map of **token symbol → list of `"AMOUNTxCOUNT"` rungs**, e.g. `CC = ["150x120"]`. The split worker
keeps `COUNT` holdings sized in `[AMOUNT, 2×AMOUNT)`. Each in‑flight quote reserves one holding of
the token the LP pays, so **your concurrency per token = its ready rungs**. Provide one entry for
every token that appears in your markets.

### `[[markets]]` and `[markets.rfq]` / `[markets.rfq.v2]`

| Key | Default | Meaning |
| --- | --- | --- |
| `markets.market_id` | — (**required**) | e.g. `CC-USDC` (devnet) / `CC-USDCx` (mainnet) |
| `markets.enabled` | `true` | Enable this market |
| `markets.rfq.enabled` | `true` | Enable RFQ on this market (**required for Atomic DVP**) |
| `markets.rfq.min_quantity` | — (**required**) | Smallest base quantity you'll quote |
| `markets.rfq.max_quantity` | — (**required**) | Largest base quantity you'll quote |
| `markets.rfq.bid_spread_percent` | `0.5` | Spread when the user sells / LP buys |
| `markets.rfq.offer_spread_percent` | `0.5` | Spread when the user buys / LP sells |
| `markets.rfq.disable_overload_spread_widening` | `false` | Pin spread — ignore sequencer‑load widening |
| `markets.rfq.disable_depletion_spread_widening` | `false` | Pin spread — ignore one‑sided depletion widening |
| `markets.rfq.allocate_before_secs` | `900` | Allocation deadline; must be `> 0` and `<` settle |
| `markets.rfq.settle_before_secs` | `1800` | Settlement deadline; must be `>` allocate |
| `markets.rfq.v2.enabled` | `false` | **Set `true` to serve Atomic DVP on this market** |

> **Omitting `bid_levels` / `offer_levels` is what gives you "no orders".** They default to empty,
> so this Atomic‑DVP‑only config never posts resting order‑book orders.

---

# Troubleshooting

- **`ATOMIC_QUOTE_PRIVATE_KEY` missing / invalid.** The agent won't start with
  `[liquidity_provider.rfq_v2].enabled = true` unless a valid secp256k1 key is in `.env`. Run
  `./cloud-agent atomic keygen` and paste the printed line. Confirm it's loaded by re‑running
  `atomic keygen` (it should say "is set and valid") — and make sure you're in the directory that
  contains `.env`.

- **`atomic status` says a venue's key DOES NOT MATCH.** Your local quote key differs from the one
  the venue was created with. Point the venue at your current key:
  ```bash
  ./cloud-agent atomic venue rotate-key --market CC-USDC
  ```
  ⚠ This invalidates all live signed envelopes for that market. Then re‑run `atomic status`.

- **A token shows no ladder rungs / low concurrency.** That token is underfunded. Fund the party
  (devnet: `faucet get`; mainnet: transfer in) and re‑run `./cloud-agent atomic setup` — the split
  worker fills the ladder. This is a silent no‑op when unfunded, never an error.

- **`atomic setup` skips venues ("No `[markets.rfq.v2]`-enabled markets").** A market only counts
  for Atomic DVP when **both** `[markets.rfq] enabled = true` **and** `[markets.rfq.v2] enabled = true`,
  and `[liquidity_provider.rfq_v2] enabled = true`. Check all three.

- **Startup error about deadlines.** Each market requires
  `0 < allocate_before_secs < settle_before_secs`. The defaults (`900` / `1800`) satisfy this.

- **`atomic setup` can't read `atomic-dvp-service.json`.** This file is **required** by
  `atomic setup`. For devnet, download it as shown in Step A6 (it's in the repo at
  `examples/atomic-dvp/devnet/atomic-dvp-service.json`); for mainnet, get it from Silvana. Put it
  in the folder you run `atomic setup` from. (`atomic status` does not need it.)

- **About RFQ V1.** Because this is an LP, the agent also opens the V1 settlement stream — it is
  used for settlement‑lifecycle coordination. There is no config switch to hard‑disable V1 while
  V2 is on; configuring only `[markets.rfq.v2]` (no grid orders) and creating venues is what makes
  swaps settle through the Atomic DVP path.

---

# Command cheat sheet

```bash
# 1. Download (macOS Apple Silicon shown — pick your platform's file in Step A1)
curl -LO https://github.com/SilvanaOne/silvana-book-agent/releases/latest/download/cloud-agent-macos-silicon.tar.gz
tar -xzf cloud-agent-macos-silicon.tar.gz
chmod +x cloud-agent

# 2. Onboard   (devnet code INVITE1 shown; mainnet: use your own code + --rpc https://orderbook-mainnet.silvana.dev:443)
./cloud-agent onboard --agent-name my-lp --email me@example.com --invite-code INVITE1

# 3. Atomic DVP quote key → paste ATOMIC_QUOTE_PRIVATE_KEY into .env
./cloud-agent atomic keygen

# 4. Check balance (devnet is pre-funded; mainnet = send tokens in yourself)
./cloud-agent info balance

# 5. Edit agent.toml  (paste the full example from Step A5 / B5)

# 6-7. Set up + check
./cloud-agent atomic setup
./cloud-agent atomic status

# 8. Run
./cloud-agent agent
```
