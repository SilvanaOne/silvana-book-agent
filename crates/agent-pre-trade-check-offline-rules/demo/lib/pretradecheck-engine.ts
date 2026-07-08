// Port of agent-pre-trade-check-offline-rules rule engine to TypeScript. Mirrors
// crates/agent-pre-trade-check-offline-rules/src/main.rs (fn evaluate).
//
// Pure offline rule engine: validate each generated order against a Rules
// document. accept if no rules fail, reject otherwise.

export type Side = "buy" | "sell";
export type Decision = "accept" | "reject";

export type Rules = Readonly<{
  maxNotionalPerOrder?: number;
  maxQuantityPerOrder?: number;
  minPrice?: number;
  maxPrice?: number;
  blockedMarkets?: readonly string[];
  allowedMarkets?: readonly string[];
  allowedSides?: readonly Side[];
}>;

export const DEFAULT_RULES: Rules = {
  maxNotionalPerOrder: 5000,
  maxQuantityPerOrder: 100,
  minPrice: 0.5,
  maxPrice: 5.0,
  allowedMarkets: ["CC-USDC", "BTC-USD"],
  allowedSides: ["buy", "sell"],
};

// Markets from which the simulator draws — includes one blocked-by-default
// market so operators can see the rule engine catch it.
export const CANDIDATE_MARKETS = ["CC-USDC", "BTC-USD", "XXX-YYY"] as const;

export type PreTradeCheckConfig = Readonly<{
  rules: Rules;
  orderArrivalPerTick: number; // Poisson lambda per tick; e.g. 0.5
  startingPrice: number;       // seed mid for the price simulator
}>;

export type OrderCheck = Readonly<{
  seq: number;
  t: number;
  market: string;
  side: Side;
  quantity: number;
  price: number;
  notional: number;
  decision: Decision;
  failedRules: readonly string[];
  accepted: boolean;
}>;

export type PreTradeCheckStats = {
  total: number;
  accepted: number;
  rejected: number;
  byFailedRule: Record<string, number>;
};

export type PreTradeCheckState = {
  config: PreTradeCheckConfig;
  status: "monitoring" | "idle";
  currentPrice: number;
  checks: OrderCheck[];
  stats: PreTradeCheckStats;
  totalPlaced: number;
};

const MAX_CHECKS = 30;

export function initState(config: PreTradeCheckConfig, startPrice: number): PreTradeCheckState {
  return {
    config,
    status: "monitoring",
    currentPrice: startPrice,
    checks: [],
    stats: { total: 0, accepted: 0, rejected: 0, byFailedRule: {} },
    totalPlaced: 0,
  };
}

/** All failed-rule tokens the engine can emit. */
export const FAILED_RULE_TOKENS = [
  "max_notional",
  "max_quantity",
  "min_price",
  "max_price",
  "blocked_market",
  "not_allowed_market",
  "not_allowed_side",
] as const;

export type FailedRule = (typeof FAILED_RULE_TOKENS)[number];

/** Evaluate a single order against Rules. Pure — no state mutation. */
export function evaluate(
  market: string,
  side: Side,
  quantity: number,
  price: number,
  rules: Rules,
): { decision: Decision; failedRules: FailedRule[]; notional: number } {
  const notional = quantity * price;
  const failedRules: FailedRule[] = [];

  if (rules.maxNotionalPerOrder !== undefined && notional > rules.maxNotionalPerOrder) {
    failedRules.push("max_notional");
  }
  if (rules.maxQuantityPerOrder !== undefined && quantity > rules.maxQuantityPerOrder) {
    failedRules.push("max_quantity");
  }
  if (rules.minPrice !== undefined && price < rules.minPrice) {
    failedRules.push("min_price");
  }
  if (rules.maxPrice !== undefined && price > rules.maxPrice) {
    failedRules.push("max_price");
  }
  if (rules.blockedMarkets && rules.blockedMarkets.includes(market)) {
    failedRules.push("blocked_market");
  }
  if (rules.allowedMarkets && rules.allowedMarkets.length > 0 && !rules.allowedMarkets.includes(market)) {
    failedRules.push("not_allowed_market");
  }
  if (rules.allowedSides && rules.allowedSides.length > 0 && !rules.allowedSides.includes(side)) {
    failedRules.push("not_allowed_side");
  }

  return {
    decision: failedRules.length === 0 ? "accept" : "reject",
    failedRules,
    notional,
  };
}

/**
 * Poisson draw with mean `lambda` — used to decide how many synthetic orders
 * arrive on a given tick. Small lambdas dominate so we keep the classic
 * inverse-CDF loop.
 */
function samplePoisson(lambda: number): number {
  if (lambda <= 0) return 0;
  const L = Math.exp(-lambda);
  let k = 0;
  let p = 1;
  do {
    k += 1;
    p *= Math.random();
  } while (p > L);
  return k - 1;
}

/** Applies a tick. Returns state + emitted events. */
export function step(
  state: PreTradeCheckState,
  price: number,
  now: number,
): { state: PreTradeCheckState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;

  const n = samplePoisson(state.config.orderArrivalPerTick);
  for (let i = 0; i < n; i++) {
    const seq = state.totalPlaced + 1;
    state.totalPlaced = seq;

    const market = CANDIDATE_MARKETS[Math.floor(Math.random() * CANDIDATE_MARKETS.length)];
    const side: Side = Math.random() < 0.5 ? "buy" : "sell";
    // Quantity 1–200 uniform (some intentionally trip max_quantity=100).
    const quantity = Math.round((1 + Math.random() * 199) * 100) / 100;
    // Price randomly around the current mid but occasionally out of band
    // so min_price / max_price triggers show up naturally.
    const jitterPct = -0.5 + Math.random() * 1.5; // -50% .. +100%
    const rawPrice = price * (1 + jitterPct);
    const outlier = Math.random() < 0.08;
    const finalPrice = outlier
      ? (Math.random() < 0.5 ? 0.05 + Math.random() * 0.4 : 5.2 + Math.random() * 1.5)
      : Math.max(0.05, rawPrice);
    const priceRounded = Math.round(finalPrice * 10000) / 10000;

    const { decision, failedRules, notional } = evaluate(market, side, quantity, priceRounded, state.config.rules);
    const accepted = decision === "accept";

    const check: OrderCheck = {
      seq,
      t: now,
      market,
      side,
      quantity,
      price: priceRounded,
      notional: Math.round(notional * 100) / 100,
      decision,
      failedRules,
      accepted,
    };
    state.checks.push(check);
    if (state.checks.length > MAX_CHECKS) state.checks.shift();

    state.stats.total += 1;
    if (accepted) state.stats.accepted += 1;
    else state.stats.rejected += 1;
    for (const rule of failedRules) {
      state.stats.byFailedRule[rule] = (state.stats.byFailedRule[rule] ?? 0) + 1;
    }

    const tail = accepted ? "ACCEPT" : `REJECT (${failedRules.join(", ")})`;
    events.push(
      `CHECK #${seq}: ${side.toUpperCase()} ${fmt(quantity)}@${fmt(priceRounded)} ${market} — ${tail}`,
    );
  }

  return { state, events };
}

function fmt(n: number): string {
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 6;
  return n.toFixed(digits);
}

/** Parse a user-supplied JSON string into a validated Rules object. */
export function parseRules(raw: string): Rules {
  const parsed = JSON.parse(raw) as Record<string, unknown>;
  const rules: {
    maxNotionalPerOrder?: number;
    maxQuantityPerOrder?: number;
    minPrice?: number;
    maxPrice?: number;
    blockedMarkets?: string[];
    allowedMarkets?: string[];
    allowedSides?: Side[];
  } = {};

  const numField = (key: string) => {
    const v = parsed[key];
    if (v === undefined || v === null) return undefined;
    const n = typeof v === "number" ? v : Number(v);
    if (!Number.isFinite(n)) throw new Error(`${key} must be a number`);
    if (n <= 0) throw new Error(`${key} must be > 0`);
    return n;
  };
  const arrField = (key: string): string[] | undefined => {
    const v = parsed[key];
    if (v === undefined || v === null) return undefined;
    if (!Array.isArray(v)) throw new Error(`${key} must be an array`);
    return v.map((x) => String(x));
  };

  const mn = numField("maxNotionalPerOrder");
  if (mn !== undefined) rules.maxNotionalPerOrder = mn;
  const mq = numField("maxQuantityPerOrder");
  if (mq !== undefined) rules.maxQuantityPerOrder = mq;
  const minP = numField("minPrice");
  if (minP !== undefined) rules.minPrice = minP;
  const maxP = numField("maxPrice");
  if (maxP !== undefined) rules.maxPrice = maxP;

  const bm = arrField("blockedMarkets");
  if (bm !== undefined) rules.blockedMarkets = bm;
  const am = arrField("allowedMarkets");
  if (am !== undefined) rules.allowedMarkets = am;

  const as = arrField("allowedSides");
  if (as !== undefined) {
    const cleaned = as.map((s) => s.toLowerCase());
    for (const s of cleaned) {
      if (s !== "buy" && s !== "sell") throw new Error(`allowedSides entries must be "buy" or "sell", got "${s}"`);
    }
    rules.allowedSides = cleaned as Side[];
  }

  return rules;
}

/** Count of active rules for display purposes. */
export function rulesCount(r: Rules): number {
  let c = 0;
  if (r.maxNotionalPerOrder !== undefined) c += 1;
  if (r.maxQuantityPerOrder !== undefined) c += 1;
  if (r.minPrice !== undefined) c += 1;
  if (r.maxPrice !== undefined) c += 1;
  if (r.blockedMarkets && r.blockedMarkets.length > 0) c += 1;
  if (r.allowedMarkets && r.allowedMarkets.length > 0) c += 1;
  if (r.allowedSides && r.allowedSides.length > 0) c += 1;
  return c;
}
