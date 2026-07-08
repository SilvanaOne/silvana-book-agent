// Port of agent-audit-replay-back-test. Reads a synthetic trading-history in memory
// and replays each order.created / settlement.settled event through a
// rule set (blocked_markets, max_notional, min/max_price, allowed_sides,
// per-market daily notional cap). Emits per-event verdict + summary.

export type Kind = "order.created" | "order.filled" | "order.cancelled" | "settlement.settled" | "settlement.failed" | "settlement.proposal";

export type Rules = Readonly<{
  maxNotionalPerOrder?: number;
  maxQuantityPerOrder?: number;
  minPrice?: number;
  maxPrice?: number;
  blockedMarkets: string[];
  allowedMarkets: string[];
  allowedSides: string[];
  perMarketMaxDailyNotional: Record<string, number>;
}>;

export type HistoryRecord = Readonly<{
  seq: number;
  ts: string;
  kind: Kind;
  payload: Record<string, unknown>;
}>;

export type Verdict = "accept" | "reject";
export type EvalRecord = Readonly<{
  seq: number;
  ts: string;
  eventKind: Kind;
  verdict: Verdict;
  hits: string[];
  market: string;
  side: string;
  notional: number;
}>;

export type ReplayConfig = Readonly<{
  historySize: number;
  parties: string[];
  markets: string[];
  rules: Rules;
  emitAccepts: boolean;
}>;

export type ReplayState = {
  config: ReplayConfig;
  history: HistoryRecord[];
  verdicts: EvalRecord[];         // bounded ~120
  total: number;
  accepts: number;
  rejects: number;
  hitsByRule: Record<string, number>;
  runs: number;
};

const KINDS: Kind[] = ["order.created", "order.filled", "order.cancelled", "settlement.settled", "settlement.failed", "settlement.proposal"];
const MAX_VERDICTS = 120;

export function initState(config: ReplayConfig): ReplayState {
  const history = genHistory(config);
  return { config, history, verdicts: [], total: 0, accepts: 0, rejects: 0, hitsByRule: {}, runs: 0 };
}

export function runReplay(state: ReplayState): { state: ReplayState; log: string[] } {
  const log: string[] = [];
  const verdicts: EvalRecord[] = [];
  const dailyByMarket: Record<string, { day: string; notional: number }> = {};
  const hitsByRule: Record<string, number> = {};
  let accepts = 0, rejects = 0, total = 0;
  for (const rec of state.history) {
    if (rec.kind !== "order.created" && rec.kind !== "settlement.settled") continue;
    total += 1;
    const day = rec.ts.slice(0, 10);
    const market = (rec.payload.market_id as string) ?? "";
    const side = ((rec.payload.side as string) ?? "").toLowerCase();
    const price = numField(rec.payload, "price");
    const quantity = numField(rec.payload, "quantity");
    const notional = ("settlement_price" in rec.payload && "base_quantity" in rec.payload)
      ? numField(rec.payload, "settlement_price") * numField(rec.payload, "base_quantity")
      : price * quantity;

    const r = state.config.rules;
    const hits: string[] = [];
    if (r.blockedMarkets.length > 0 && r.blockedMarkets.includes(market)) hits.push("blocked_market");
    if (r.allowedMarkets.length > 0 && !r.allowedMarkets.includes(market)) hits.push("market_not_allowed");
    if (r.allowedSides.length > 0 && side.length > 0 && !r.allowedSides.some((s) => s.toLowerCase() === side)) hits.push("side_not_allowed");
    if (r.maxNotionalPerOrder !== undefined && notional > r.maxNotionalPerOrder) hits.push("max_notional_per_order");
    if (r.maxQuantityPerOrder !== undefined && quantity > r.maxQuantityPerOrder) hits.push("max_quantity_per_order");
    if (r.minPrice !== undefined && price > 0 && price < r.minPrice) hits.push("min_price");
    if (r.maxPrice !== undefined && price > r.maxPrice) hits.push("max_price");
    if (r.perMarketMaxDailyNotional[market] !== undefined) {
      const key = market;
      const daily = dailyByMarket[key];
      if (!daily || daily.day !== day) dailyByMarket[key] = { day, notional: 0 };
      dailyByMarket[key].notional += notional;
      if (dailyByMarket[key].notional > r.perMarketMaxDailyNotional[market]) hits.push(`per_market_max_daily_notional[${market}]`);
    }

    const verdict: Verdict = hits.length === 0 ? "accept" : "reject";
    if (verdict === "accept") accepts += 1; else rejects += 1;
    for (const h of hits) hitsByRule[h] = (hitsByRule[h] ?? 0) + 1;
    if (verdict === "reject" || state.config.emitAccepts) {
      verdicts.push({ seq: rec.seq, ts: rec.ts, eventKind: rec.kind, verdict, hits, market, side: side.toUpperCase(), notional });
      if (verdicts.length > MAX_VERDICTS) verdicts.shift();
    }
  }
  state.verdicts = verdicts;
  state.total = total;
  state.accepts = accepts;
  state.rejects = rejects;
  state.hitsByRule = hitsByRule;
  state.runs += 1;
  log.push(`REPLAY run #${state.runs}: ${total} evaluated · ${accepts} accept · ${rejects} reject · ${Object.keys(hitsByRule).length} rule kinds`);
  return { state, log };
}

export function reloadHistory(state: ReplayState): { state: ReplayState; log: string[] } {
  state.history = genHistory(state.config);
  state.verdicts = [];
  state.total = 0; state.accepts = 0; state.rejects = 0;
  state.hitsByRule = {};
  return runReplay(state);
}

function genHistory(config: ReplayConfig): HistoryRecord[] {
  const out: HistoryRecord[] = [];
  const now = Date.now();
  for (let i = 0; i < config.historySize; i++) {
    const kind = KINDS[Math.floor(Math.random() * KINDS.length)];
    const market = config.markets[Math.floor(Math.random() * config.markets.length)];
    const ts = new Date(now - (config.historySize - i) * 30000).toISOString();
    let payload: Record<string, unknown>;
    if (kind === "order.created" || kind === "order.filled" || kind === "order.cancelled") {
      payload = { market_id: market, order_id: i + 1, side: Math.random() < 0.5 ? "BID" : "OFFER",
        price: Math.round((0.5 + Math.random() * 60000) * 10000) / 10000,
        quantity: Math.round((0.5 + Math.random() * 50) * 100) / 100 };
    } else {
      const b = config.parties[Math.floor(Math.random() * config.parties.length)];
      let s = config.parties[Math.floor(Math.random() * config.parties.length)];
      if (s === b) s = config.parties[(config.parties.indexOf(b) + 1) % config.parties.length];
      payload = { market_id: market, buyer: b, seller: s,
        settlement_price: Math.round((0.5 + Math.random() * 60000) * 10000) / 10000,
        base_quantity: Math.round((0.5 + Math.random() * 20) * 100) / 100,
        proposal_id: `prop_${i}` };
    }
    out.push({ seq: i + 1, ts, kind, payload });
  }
  return out;
}

function numField(payload: Record<string, unknown>, key: string): number {
  const v = payload[key];
  if (typeof v === "number") return v;
  if (typeof v === "string") { const n = Number(v); return Number.isFinite(n) ? n : 0; }
  return 0;
}
