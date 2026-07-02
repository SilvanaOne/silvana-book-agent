// Port of agent-oracle to TypeScript. Mirrors
// crates/agent-oracle/src/main.rs (fn run + fn payload).
//
// Behavior:
//   - Poll every `pollSecs` seconds.
//   - For each market, fetch a price (from `source`) and emit one publish record.
//   - Publish record: { t, market, price, source }.
//
// In this demo prices are simulated per market via an internal GBM random walk,
// stepped every UI tick (1s). Publish records are emitted only every pollSecs.

export type OracleSource = "binance_spot" | "bybit" | "coingecko";

export type OracleConfig = Readonly<{
  markets: string[];               // ordered market list, e.g. ["CC-USDC", "BTC-USD"]
  source: OracleSource;            // upstream price source label
  pollSecs: number;                // publish every N seconds (>= 1)
  startingPrices: number[];        // one starting price per market (length must match)
}>;

export type MarketPrice = Readonly<{ market: string; price: number }>;
export type PublishRecord = Readonly<{ t: number; market: string; price: number; source: OracleSource }>;

export type OracleState = {
  config: OracleConfig;
  status: "publishing" | "idle";
  currentPrices: MarketPrice[];    // updates every tick (walk)
  publishedCount: number;          // total records emitted
  lastPublishedAt: number | null;
  nextPublishAt: number;           // epoch ms of next publish
  publishedRecords: PublishRecord[]; // bounded, most recent MAX_RECORDS
};

const MAX_RECORDS = 60;

export function initState(config: OracleConfig, now: number): OracleState {
  const prices: MarketPrice[] = config.markets.map((m, i) => ({
    market: m,
    price: config.startingPrices[i] ?? config.startingPrices[0] ?? 1,
  }));
  return {
    config,
    status: "publishing",
    currentPrices: prices,
    publishedCount: 0,
    lastPublishedAt: null,
    nextPublishAt: now + config.pollSecs * 1000,
    publishedRecords: [],
  };
}

/**
 * Advance one tick. `price` is ignored (each market walks independently).
 * Returns state + emitted events (log lines).
 */
export function step(state: OracleState, _price: number, now: number): { state: OracleState; events: string[] } {
  if (state.status !== "publishing") return { state, events: [] };
  const events: string[] = [];

  // Walk each market independently — GBM with small vol.
  state.currentPrices = state.currentPrices.map((mp) => ({
    market: mp.market,
    price: gbmStep(mp.price),
  }));

  // Publish window elapsed? Emit one record per market.
  if (now >= state.nextPublishAt) {
    for (const mp of state.currentPrices) {
      const rec: PublishRecord = { t: now, market: mp.market, price: mp.price, source: state.config.source };
      state.publishedRecords.push(rec);
      state.publishedCount += 1;
      events.push(`PUBLISH [${state.config.source}] ${mp.market}: ${fmt(mp.price)} at ${new Date(now).toISOString()}`);
    }
    while (state.publishedRecords.length > MAX_RECORDS) state.publishedRecords.shift();
    state.lastPublishedAt = now;
    state.nextPublishAt = now + state.config.pollSecs * 1000;
  }

  return { state, events };
}

function gbmStep(current: number): number {
  const u1 = Math.max(1e-9, Math.random());
  const u2 = Math.random();
  const z = Math.sqrt(-2 * Math.log(u1)) * Math.cos(2 * Math.PI * u2);
  const factor = 1 + 0.005 * z; // ~0.5% stddev per tick
  const next = current * factor;
  return next > 0 ? next : current * 0.5;
}

function fmt(n: number): string {
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 6;
  return n.toFixed(digits);
}
