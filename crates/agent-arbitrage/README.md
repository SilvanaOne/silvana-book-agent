# Arbitrage Agent

A Silvana-hosted trading agent that watches multiple exchanges and trading venues,
finds price differences for the same assets, and runs automated **dual trades** —
buying where the price is lower and selling where it is higher.

The agent is built for operators who want continuous spread monitoring with a
clear console, configurable risk limits, and a safe **paper mode** for testing
before any live trading.

## What it does

- **Scans** connected venues on a schedule and keeps an up-to-date view of prices.
- **Detects** spreads — situations where the same asset (or equivalent stablecoin
  pairs) trades at different prices on different venues.
- **Trades** in two coordinated legs when an opportunity passes your thresholds:
  one leg to buy, one leg to sell, aiming to capture the difference net of fees.
- **Reports** activity, spreads, and results through a web-based operator console.

## How the scanner works

The scanner is the always-on part of the agent. It repeatedly requests quotes or
order-book data from each connected venue, normalizes prices into comparable units,
and compares them across venues and asset pairs.

When it finds a gap large enough to be interesting, it evaluates whether the
opportunity is still viable after fees, minimum size, and any conversion between
related assets (for example between different USD-pegged instruments). Viable
spreads are ranked, logged, and made available to the trading logic and the
operator dashboard in near real time.

The scanner does not place orders on its own — it **finds and signals**
opportunities. Trading decisions follow the rules and limits you configure.

## How dual-trade works

A **dual trade** is two linked actions executed as one arbitrage attempt:

1. **Buy leg** — acquire the asset on the venue where it is cheaper.
2. **Sell leg** — dispose of the same economic exposure on the venue where it
   is more expensive.

Both legs are meant to run close together so the agent is not left holding
one-sided risk for long. Size, timing, and venue health are checked before
execution. If limits are exceeded, the kill switch is on, or an opportunity
expires, the trade is skipped or halted.

In **paper mode**, the full flow runs without real money — spreads are detected,
trades are simulated, and results appear in the console so you can validate
behavior before going live.

## Operator console

The web dashboard gives you a single place to:

- See **live spreads** and recent activity.
- Turn trading on or off with a **kill switch** and adjust risk settings.
- Review **stats** — where opportunities come from and how large spreads tend to be.
- Use an **AI assistant** for optional, advisory hints (suggestions only; it does
  not trade or change settings on its own).

## Getting started

From the repository root:

```bash
./run.sh
```

Then open **http://localhost:3001** in your browser.

For development without the full bootstrap script:

```bash
npm run dev
```

See `run.sh --help` for prepare-only and infrastructure options.

For more details, see [GUIDELINES.md](GUIDELINES.md).
