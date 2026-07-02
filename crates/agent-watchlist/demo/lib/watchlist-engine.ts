// Port of agent-watchlist observation logic to TypeScript. Mirrors
// crates/agent-watchlist/src/main.rs.
//
// Read-only market monitor: subscribes to StreamPrices + SubscribeOrderbook
// for a set of markets and logs every update. No orders, no settlement,
// no ledger writes. The demo simulates each market's mid with an
// independent GBM walk and synthesizes a shallow depth ladder around it.

export type WatchlistConfig = Readonly<{
  markets: string[];       // e.g. ["CC-USDC","BTC-USD","CETH-CC"]
  depthLevels: number;     // number of price levels per side (default 10)
  includeOrderbook: boolean;
  includePrices: boolean;
  pollSecs: number;        // seconds between synthesized updates
  startingPrice: number;   // seed for the first market; others are offset
}>;

export type MarketSnapshot = {
  market: string;
  price: number;
  bidPx: number;
  offerPx: number;
  spreadBps: number;
  bidDepth: number;        // total quantity across bid levels
  offerDepth: number;      // total quantity across offer levels
  startingPrice: number;
  priceChangeSinceStart: number; // percent, e.g. +1.23
  lastUpdateAt: number;    // epoch ms
  priceUpdates: number;
  depthUpdates: number;
};

export type WatchlistState = {
  config: WatchlistConfig;
  status: "monitoring" | "idle";
  snapshots: MarketSnapshot[]; // one row per market, in config order
  updatesCount: number;
  priceUpdatesCount: number;
  depthUpdatesCount: number;
  lastUpdateAt: number | null;
  startedAt: number;
};

// Deterministic-ish per-market seed offsets so demo shows visibly different
// price levels for well-known symbols. Anything else defaults to +0.
const MARKET_OFFSETS: Readonly<Record<string, number>> = {
  "CC-USDC": 1.0,
  "BTC-USD": 65000,
  "CETH-CC": 3200,
  "BTC-USDC": 65000,
  "ETH-USDC": 3200,
  "CBTC-CC": 65000,
};

export function seedMarketPrice(market: string, baseStartingPrice: number): number {
  const explicit = MARKET_OFFSETS[market];
  if (typeof explicit === "number") return explicit;
  return baseStartingPrice;
}

export function initState(config: WatchlistConfig, now: number): WatchlistState {
  const snapshots: MarketSnapshot[] = config.markets.map((m) => {
    const px = seedMarketPrice(m, config.startingPrice);
    return {
      market: m,
      price: px,
      bidPx: px * 0.999,
      offerPx: px * 1.001,
      spreadBps: 20,
      bidDepth: 0,
      offerDepth: 0,
      startingPrice: px,
      priceChangeSinceStart: 0,
      lastUpdateAt: now,
      priceUpdates: 0,
      depthUpdates: 0,
    };
  });
  return {
    config,
    status: "monitoring",
    snapshots,
    updatesCount: 0,
    priceUpdatesCount: 0,
    depthUpdatesCount: 0,
    lastUpdateAt: null,
    startedAt: now,
  };
}

// Cheap per-market GBM step. ~0.5% vol per step by default.
function walk(current: number, volPerStep = 0.005, drift = 0): number {
  const u1 = Math.max(1e-9, Math.random());
  const u2 = Math.random();
  const z = Math.sqrt(-2 * Math.log(u1)) * Math.cos(2 * Math.PI * u2);
  const factor = 1 + drift + volPerStep * z;
  const next = current * factor;
  return next > 0 ? next : current * 0.5;
}

/**
 * Applies one polling cycle. The `price` argument from the store is ignored —
 * each market walks independently. Returns updated state + emitted events
 * (log lines mirroring the Rust `[prices]` / `[depth:MKT]` messages).
 */
export function step(
  state: WatchlistState,
  _price: number,
  now: number,
): { state: WatchlistState; events: string[] } {
  if (state.status !== "monitoring") return { state, events: [] };
  const events: string[] = [];

  for (const snap of state.snapshots) {
    // Walk the mid.
    const nextMid = walk(snap.price);
    snap.price = nextMid;

    // Synthesize a spread + shallow ladder. Spread widens slightly with vol.
    const spreadFrac = 0.001 + Math.random() * 0.0008; // ~10-18 bps
    snap.bidPx = round8(nextMid * (1 - spreadFrac / 2));
    snap.offerPx = round8(nextMid * (1 + spreadFrac / 2));
    snap.spreadBps = ((snap.offerPx - snap.bidPx) / nextMid) * 10000;

    // Depth = levels × per-level quantity around a base of ~1.0. Both sides
    // roughly symmetric with small random imbalance.
    const perLevel = 1 + Math.random() * 2;
    snap.bidDepth = round4(state.config.depthLevels * perLevel);
    snap.offerDepth = round4(state.config.depthLevels * (perLevel * (0.85 + Math.random() * 0.3)));

    snap.priceChangeSinceStart = ((nextMid - snap.startingPrice) / snap.startingPrice) * 100;
    snap.lastUpdateAt = now;

    if (state.config.includePrices) {
      snap.priceUpdates += 1;
      state.priceUpdatesCount += 1;
      events.push(`PRICE ${snap.market}: ${fmt(nextMid)} bid=${fmt(snap.bidPx)} ask=${fmt(snap.offerPx)}`);
    }
    if (state.config.includeOrderbook) {
      snap.depthUpdates += 1;
      state.depthUpdatesCount += 1;
      events.push(
        `DEPTH ${snap.market}: bid×${state.config.depthLevels}(${fmt(snap.bidDepth)}) / offer×${state.config.depthLevels}(${fmt(snap.offerDepth)})`,
      );
    }
    state.updatesCount += (state.config.includePrices ? 1 : 0) + (state.config.includeOrderbook ? 1 : 0);
  }

  state.lastUpdateAt = now;
  return { state, events };
}

function round8(n: number): number {
  return Math.round(n * 1e8) / 1e8;
}
function round4(n: number): number {
  return Math.round(n * 1e4) / 1e4;
}
function fmt(n: number): string {
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 6;
  return n.toFixed(digits);
}
