// Port of agent-treasury-mgmt. Rebalances to per-instrument absolute
// quote-value targets, but routes each candidate trade through a policy:
//  - notional <= approvalThresholdQuote → auto-submit (direct)
//  - notional > approvalThresholdQuote → enqueue for human-approval
//  - notional > maxTradeQuote → refused (never happens; skip)
//  - rolling 24h cap (maxDailyTradeQuote) — refuse when exceeded
//
// This demo simulates targets vs. balances at live mids, cycles through
// rebalances, and routes each leg through the policy.

export type Target = Readonly<{
  instrument: string;
  market: string;
  targetValue: number;               // absolute quote-value target
}>;

export type TreasuryConfig = Readonly<{
  targets: Target[];
  approvalThresholdQuote: number;
  maxTradeQuote: number;
  maxDailyTradeQuote: number;
  thresholdQuote: number;            // per-instrument tolerance for triggering
  rebalanceFraction: number;         // 0..1
  checkIntervalSecs: number;         // >= 1
  startingBalances: Record<string, number>;
  startingPrices: Record<string, number>;
  balanceDriftPerCycle: number;
}>;

export type Route = "direct" | "approval" | "refused_cap";

export type Leg = Readonly<{
  seq: number;
  t: number;
  instrument: string;
  market: string;
  side: "BID" | "OFFER";
  qty: number;
  price: number;
  notional: number;
  route: Route;
  reason?: string;
}>;

export type Row = Readonly<{
  target: Target;
  balance: number;
  price: number;
  currentValue: number;
  gap: number;                       // signed target - current
  breached: boolean;
}>;

export type Snapshot = Readonly<{ seq: number; t: number; rows: Row[]; totalValue: number; legs: Leg[] }>;

export type TreasuryState = {
  config: TreasuryConfig;
  status: "running" | "idle";
  cycle: number;
  balances: Record<string, number>;
  prices: Record<string, number>;
  lastSnapshot: Snapshot | null;
  legs: Leg[];                       // bounded
  directCount: number;
  approvalQueueCount: number;
  refusedCount: number;
  dailyRolling: Array<{ t: number; notional: number }>;
  nextCheckAt: number;
  nextSeq: number;
};

const MAX_LEGS = 60;

export function initState(config: TreasuryConfig, now: number): TreasuryState {
  return {
    config, status: "running", cycle: 0,
    balances: { ...config.startingBalances },
    prices: { ...config.startingPrices },
    lastSnapshot: null, legs: [], directCount: 0, approvalQueueCount: 0, refusedCount: 0,
    dailyRolling: [], nextCheckAt: now + config.checkIntervalSecs * 1000, nextSeq: 1,
  };
}

export function step(state: TreasuryState, driverPrice: number, now: number): { state: TreasuryState; log: string[] } {
  if (state.status !== "running") return { state, log: [] };
  const log: string[] = [];

  // Walk prices (anchor first market to driver).
  const markets = Object.keys(state.prices);
  const scale = markets.length > 0 ? driverPrice / (state.prices[markets[0]] ?? 1) : 1;
  for (const m of markets) {
    const anchored = m === markets[0] ? driverPrice : state.prices[m] * (1 + (scale - 1) * 0.3);
    state.prices[m] = jitter(anchored, 0.003);
  }

  // Small balance drift.
  for (const inst of Object.keys(state.balances)) {
    const delta = (Math.random() - 0.5) * state.config.balanceDriftPerCycle / 4;
    state.balances[inst] = Math.max(0, state.balances[inst] + delta);
  }

  // Purge rolling 24h cap ledger.
  const cutoff = now - 24 * 3600 * 1000;
  while (state.dailyRolling.length > 0 && state.dailyRolling[0].t < cutoff) state.dailyRolling.shift();

  if (now >= state.nextCheckAt) {
    state.cycle += 1;
    const rows: Row[] = state.config.targets.map((t) => {
      const balance = state.balances[t.instrument] ?? 0;
      const price = state.prices[t.market] ?? 0;
      const currentValue = balance * price;
      const gap = t.targetValue - currentValue;
      const breached = Math.abs(gap) > state.config.thresholdQuote;
      return { target: t, balance, price, currentValue, gap, breached };
    });
    const totalValue = rows.reduce((a, r) => a + r.currentValue, 0);
    const dailySpent = state.dailyRolling.reduce((a, e) => a + e.notional, 0);
    const dailyRemaining = state.config.maxDailyTradeQuote - dailySpent;

    const legs: Leg[] = [];
    for (const r of rows) {
      if (!r.breached) continue;
      if (r.price <= 0) continue;
      const desiredNotional = Math.abs(r.gap) * state.config.rebalanceFraction;
      let notional = Math.min(desiredNotional, state.config.maxTradeQuote);
      const side: Leg["side"] = r.gap > 0 ? "BID" : "OFFER";

      // Rolling cap.
      let route: Route;
      let reason: string | undefined;
      if (notional > dailyRemaining) {
        // Trim to the remaining daily budget; if there's nothing left, refuse.
        if (dailyRemaining <= 0) {
          route = "refused_cap"; reason = "24h cap exhausted"; notional = 0;
        } else {
          route = notional > state.config.approvalThresholdQuote ? "approval" : "direct";
          notional = dailyRemaining;
          reason = "trimmed to 24h cap";
        }
      } else {
        route = notional > state.config.approvalThresholdQuote ? "approval" : "direct";
      }

      if (notional <= 0) {
        legs.push({ seq: state.nextSeq++, t: now, instrument: r.target.instrument, market: r.target.market, side, qty: 0, price: r.price, notional: 0, route, reason });
        state.refusedCount += 1;
        log.push(`REFUSE ${r.target.instrument}@${r.target.market} ${side} — ${reason ?? "0 notional"}`);
        continue;
      }
      const qty = notional / r.price;
      const leg: Leg = { seq: state.nextSeq++, t: now, instrument: r.target.instrument, market: r.target.market, side, qty, price: r.price, notional, route, reason };
      legs.push(leg);

      if (route === "direct") {
        state.directCount += 1;
        // Simulate immediate fill for direct legs.
        if (side === "BID") state.balances[r.target.instrument] = (state.balances[r.target.instrument] ?? 0) + qty;
        else state.balances[r.target.instrument] = Math.max(0, (state.balances[r.target.instrument] ?? 0) - qty);
        state.dailyRolling.push({ t: now, notional });
        log.push(`DIRECT ${side} ${r.target.instrument}@${r.target.market} qty=${qty.toFixed(4)} @${r.price.toFixed(6)} · ${notional.toFixed(2)}`);
      } else if (route === "approval") {
        state.approvalQueueCount += 1;
        state.dailyRolling.push({ t: now, notional });   // reserve budget when enqueuing
        log.push(`APPROV ${side} ${r.target.instrument}@${r.target.market} qty=${qty.toFixed(4)} @${r.price.toFixed(6)} · ${notional.toFixed(2)} → approval queue`);
      } else {
        state.refusedCount += 1;
        log.push(`REFUSE ${r.target.instrument}@${r.target.market} ${side} — ${reason ?? "?"}`);
      }
    }

    state.legs.push(...legs);
    while (state.legs.length > MAX_LEGS) state.legs.shift();
    state.lastSnapshot = { seq: state.cycle, t: now, rows, totalValue, legs };
    if (legs.length === 0) log.push(`OK    cycle=${state.cycle} portfolio=${totalValue.toFixed(2)} — no breaches`);
    state.nextCheckAt = now + state.config.checkIntervalSecs * 1000;
  }

  return { state, log };
}

function jitter(current: number, vol: number): number {
  const u1 = Math.max(1e-9, Math.random());
  const u2 = Math.random();
  const z = Math.sqrt(-2 * Math.log(u1)) * Math.cos(2 * Math.PI * u2);
  const next = current * (1 + vol * z);
  return next > 0 ? next : current * 0.5;
}
