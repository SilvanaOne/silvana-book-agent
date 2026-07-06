// Port of agent-trade-explain. Generates a synthetic trading-history and
// runs explain_trade over every relevant record. Same rules as
// crates/agent-trade-explain/src/main.rs::explain_trade.

export type TradeKind = "order.created" | "settlement.settled";

export type Explanation = Readonly<{
  seq: number;
  forSeq: number;
  forKind: string;
  market: string;
  side: string;
  price: number;
  quantity: number;
  notional: number;
  rationale: string;
  counterfactual: string;
  ts: string;
}>;

export type ExplainConfig = Readonly<{
  historySize: number;
  markets: string[];
  includeOrders: boolean;
  includeSettlements: boolean;
  model: string;
}>;

export type ExplainState = {
  config: ExplainConfig;
  explanations: Explanation[];       // ~40
  totalHistory: number;
  totalExplained: number;
  byKind: Record<string, number>;
  byMarket: Record<string, number>;
  bySide: Record<string, number>;
  runs: number;
};

const MAX_EXPL = 40;

export function initState(config: ExplainConfig): ExplainState {
  return { config, explanations: [], totalHistory: 0, totalExplained: 0, byKind: {}, byMarket: {}, bySide: {}, runs: 0 };
}

export function runExplain(state: ExplainState): { state: ExplainState; log: string[] } {
  const history = genHistory(state.config);
  state.totalHistory = history.length;
  const out: Explanation[] = [];
  const byKind: Record<string, number> = {};
  const byMarket: Record<string, number> = {};
  const bySide: Record<string, number> = {};
  let seq = 1;
  for (const h of history) {
    const relevant =
      (h.kind === "order.created" && state.config.includeOrders) ||
      (h.kind === "settlement.settled" && state.config.includeSettlements);
    if (!relevant) continue;
    const notional = h.price * h.quantity;
    const { rationale, counterfactual } = explainTrade(h.kind, h.market, h.side, h.price, h.quantity, notional);
    const ex: Explanation = {
      seq: seq++, forSeq: h.seq, forKind: h.kind, market: h.market, side: h.side,
      price: h.price, quantity: h.quantity, notional,
      rationale, counterfactual, ts: h.ts,
    };
    out.push(ex);
    while (out.length > MAX_EXPL) out.shift();
    byKind[h.kind] = (byKind[h.kind] ?? 0) + 1;
    byMarket[h.market] = (byMarket[h.market] ?? 0) + 1;
    bySide[h.side] = (bySide[h.side] ?? 0) + 1;
  }
  state.explanations = out;
  state.totalExplained = out.length;
  state.byKind = byKind;
  state.byMarket = byMarket;
  state.bySide = bySide;
  state.runs += 1;
  return { state, log: [`RUN #${state.runs}: history=${history.length} explained=${out.length}`] };
}

type HistoryRec = { seq: number; ts: string; kind: TradeKind | string; market: string; side: string; price: number; quantity: number };
function genHistory(cfg: ExplainConfig): HistoryRec[] {
  const out: HistoryRec[] = [];
  const now = Date.now();
  const sides = ["BID", "OFFER"];
  const kinds: (TradeKind | "order.filled")[] = ["order.created", "order.filled", "settlement.settled"];
  for (let i = 0; i < cfg.historySize; i++) {
    const kind = kinds[Math.floor(Math.random() * kinds.length)];
    const market = cfg.markets[Math.floor(Math.random() * cfg.markets.length)];
    const side = sides[Math.floor(Math.random() * sides.length)];
    const priceBase = market.startsWith("BTC") ? 60000 : market.startsWith("ETH") ? 3500 : 1.02;
    const price = Math.round(priceBase * (0.95 + Math.random() * 0.1) * 10000) / 10000;
    const quantity = Math.round((0.1 + Math.random() * 30) * 100) / 100;
    const ts = new Date(now - (cfg.historySize - i) * 30_000).toISOString();
    out.push({ seq: i + 1, ts, kind, market, side, price, quantity });
  }
  return out;
}

function explainTrade(kind: string, market: string, side: string, price: number, qty: number, notional: number): { rationale: string; counterfactual: string } {
  const direction = side === "BID" || side.toLowerCase() === "buy" ? "buy" : "sell";
  const sizeClass = notional > 10000 ? "large" : notional > 1000 ? "medium" : "small";
  const base = market.split("-")[0];

  let rationale: string;
  let counterfactual: string;
  if (kind === "order.created") {
    rationale = `${cap(sizeClass)} ${direction} order placed on ${market} at ${price} (${qty.toFixed(2)} units, ${sizeClass} notional ${notional.toFixed(2)}). Rationale: signals from upstream agents indicated favourable ${direction === "buy" ? "bullish" : "bearish"} pressure on this market at the time.`;
    counterfactual = direction === "buy"
      ? `Skipping this buy would have kept ${notional.toFixed(2)} ${base} in cash-equivalent — with the observed subsequent price move, that would have under-hedged the position by roughly ${(notional * 0.005).toFixed(2)}.`
      : `Skipping this sell would have left ${notional.toFixed(2)} of exposure on the book; with the observed subsequent move, unrealized P&L would have shifted by approximately ${(notional * 0.008).toFixed(2)}.`;
  } else {
    rationale = `Settlement completed on ${market} (${qty.toFixed(2)} × ${price} = ${notional.toFixed(2)} notional). The trade executed as intended; no failures or partial fills recorded downstream.`;
    counterfactual = `Had settlement failed, the linked orders on ${market} would need cancellation and reposting — additional latency + slippage estimated at ~${(notional * 0.003).toFixed(2)}.`;
  }
  return { rationale, counterfactual };
}

function cap(s: string) { return s.length === 0 ? s : s[0].toUpperCase() + s.slice(1); }
