// Port of agent-smart-allocation-buckets to TypeScript. Mirrors
// crates/agent-smart-allocation-buckets/src/main.rs (fn alloc_loop) — every cycle:
//   1. Value every instrument at its market mid,
//   2. Aggregate into buckets (each with a target bucket-level weight,
//      and each with within-bucket instrument weights),
//   3. For any bucket whose current weight deviates from target by more than
//      `bucketThresholdPct`, place rebalance orders spread across its
//      instruments (BID for underweight, OFFER for overweight), sized by
//      `rebalanceFraction × deviation × portfolioValue`.
//
// The demo simulates all instrument prices walking each tick + occasional
// balance drift from an unrelated strategy so bucket weights actually move.

export type InstrumentSlot = Readonly<{
  instrument: string;
  market: string;
  weight: number;              // within-bucket target weight (already normalised)
}>;

export type Bucket = Readonly<{
  name: string;
  weight: number;              // portfolio-level target weight (already normalised)
  instruments: InstrumentSlot[];
}>;

export type SmartAllocConfig = Readonly<{
  buckets: Bucket[];
  bucketThresholdPct: number;  // e.g. 2.0 = trigger when |current - target| > 2 pct
  rebalanceFraction: number;   // 0..1 — how much of the gap to close per cycle
  checkIntervalSecs: number;   // >= 1
  startingBalances: Record<string, number>;   // instrument → starting balance
  startingPrices: Record<string, number>;     // market → starting mid
  balanceDriftPerCycle: number;               // ±drift each cycle to keep it interesting
}>;

export type RebalanceLeg = Readonly<{
  bucket: string;
  instrument: string;
  market: string;
  side: "BID" | "OFFER";
  qty: number;
  price: number;
  notional: number;
  deltaPct: number;
}>;

export type AllocationSnapshot = Readonly<{
  seq: number;
  t: number;
  portfolioValue: number;
  bucketRows: Array<{
    name: string;
    targetWeight: number;
    currentWeight: number;
    value: number;
    deviationPct: number;
    breached: boolean;
    instruments: Array<{
      instrument: string;
      market: string;
      targetWeight: number;
      currentWeight: number;   // within-bucket
      balance: number;
      price: number;
      value: number;
    }>;
  }>;
  legs: RebalanceLeg[];
}>;

export type SmartAllocState = {
  config: SmartAllocConfig;
  status: "running" | "idle";
  cycle: number;
  balances: Record<string, number>;
  prices: Record<string, number>;
  totalRebalances: number;
  totalLegs: number;
  totalNotional: number;
  lastSnapshot: AllocationSnapshot | null;
  weightHistory: Array<{ t: number; weights: Record<string, number> }>;
  nextCheckAt: number;
};

const MAX_HISTORY = 240;

export function initState(config: SmartAllocConfig, now: number): SmartAllocState {
  const balances = { ...config.startingBalances };
  const prices = { ...config.startingPrices };
  for (const b of config.buckets) {
    for (const i of b.instruments) {
      if (!(i.instrument in balances)) balances[i.instrument] = 0;
      if (!(i.market in prices)) prices[i.market] = 1;
    }
  }
  return {
    config,
    status: "running",
    cycle: 0,
    balances,
    prices,
    totalRebalances: 0,
    totalLegs: 0,
    totalNotional: 0,
    lastSnapshot: null,
    weightHistory: [],
    nextCheckAt: now + config.checkIntervalSecs * 1000,
  };
}

/** Advance one tick. `driverPrice` is a multiplicative driver for prices. */
export function step(state: SmartAllocState, driverPrice: number, now: number): { state: SmartAllocState; log: string[] } {
  if (state.status !== "running") return { state, log: [] };
  const log: string[] = [];

  // 1. Walk every price. Anchor first price to driver, others jitter independently.
  const markets = Object.keys(state.prices);
  const scale = markets.length > 0 ? driverPrice / (state.prices[markets[0]] ?? 1) : 1;
  for (const m of markets) {
    const anchored = m === markets[0] ? driverPrice : state.prices[m] * (1 + (scale - 1) * 0.4);
    state.prices[m] = jitter(anchored, 0.003);
  }

  // 2. Randomly nudge balances a little each tick (an unrelated strategy pushing exposure).
  for (const instrument of Object.keys(state.balances)) {
    const delta = (Math.random() - 0.5) * state.config.balanceDriftPerCycle / 4;
    state.balances[instrument] = Math.max(0, state.balances[instrument] + delta);
  }

  // 3. Periodic policy evaluation.
  if (now >= state.nextCheckAt) {
    const snap = evaluate(state, now);
    state.lastSnapshot = snap;
    // Apply fills: pretend rebalance legs settle immediately.
    if (snap.legs.length > 0) {
      state.totalRebalances += 1;
      state.totalLegs += snap.legs.length;
      state.totalNotional += snap.legs.reduce((a, l) => a + l.notional, 0);
      for (const leg of snap.legs) {
        if (leg.side === "BID") state.balances[leg.instrument] = (state.balances[leg.instrument] ?? 0) + leg.qty;
        else state.balances[leg.instrument] = Math.max(0, (state.balances[leg.instrument] ?? 0) - leg.qty);
        log.push(`REBAL ${leg.side} ${leg.instrument}@${leg.market} qty=${leg.qty.toFixed(4)} @${leg.price.toFixed(6)} (bucket=${leg.bucket} Δ=${leg.deltaPct.toFixed(2)}%)`);
      }
    } else {
      const breachedBuckets = snap.bucketRows.filter((r) => r.breached).map((r) => r.name);
      if (breachedBuckets.length === 0) log.push(`OK    cycle=${snap.seq}  portfolio=${snap.portfolioValue.toFixed(2)} — all buckets inside threshold`);
      else log.push(`SKIP  cycle=${snap.seq}  ${breachedBuckets.join(",")} breached but 0 legs (weights=0 or missing mid)`);
    }
    state.cycle = snap.seq;
    // Store weights history for chart.
    const weights: Record<string, number> = {};
    for (const b of snap.bucketRows) weights[b.name] = b.currentWeight;
    state.weightHistory.push({ t: now, weights });
    while (state.weightHistory.length > MAX_HISTORY) state.weightHistory.shift();
    state.nextCheckAt = now + state.config.checkIntervalSecs * 1000;
  }

  return { state, log };
}

function evaluate(state: SmartAllocState, now: number): AllocationSnapshot {
  const bucketRows: AllocationSnapshot["bucketRows"] = [];
  let portfolioValue = 0;

  for (const bucket of state.config.buckets) {
    let value = 0;
    const instrumentRows = bucket.instruments.map((i) => {
      const bal = state.balances[i.instrument] ?? 0;
      const price = state.prices[i.market] ?? 0;
      const v = bal * price;
      value += v;
      return { instrument: i.instrument, market: i.market, targetWeight: i.weight, currentWeight: 0, balance: bal, price, value: v };
    });
    for (const row of instrumentRows) {
      row.currentWeight = value > 0 ? row.value / value : 0;
    }
    bucketRows.push({
      name: bucket.name,
      targetWeight: bucket.weight,
      currentWeight: 0,
      value,
      deviationPct: 0,
      breached: false,
      instruments: instrumentRows,
    });
    portfolioValue += value;
  }

  for (const row of bucketRows) {
    row.currentWeight = portfolioValue > 0 ? row.value / portfolioValue : 0;
    row.deviationPct = (row.currentWeight - row.targetWeight) * 100;
    row.breached = Math.abs(row.deviationPct) > state.config.bucketThresholdPct;
  }

  // Generate rebalance legs for breached buckets.
  const legs: RebalanceLeg[] = [];
  const cycle = state.cycle + 1;
  for (const row of bucketRows) {
    if (!row.breached) continue;
    const delta = row.currentWeight - row.targetWeight;         // positive → overweight
    const notional = Math.abs(delta) * state.config.rebalanceFraction * portfolioValue;
    if (notional <= 0) continue;
    for (const inst of row.instruments) {
      if (inst.targetWeight <= 0) continue;
      const price = state.prices[inst.market] ?? 0;
      if (price <= 0) continue;
      const slotNotional = notional * inst.targetWeight;
      const qty = slotNotional / price;
      if (qty <= 0) continue;
      const side: "BID" | "OFFER" = delta > 0 ? "OFFER" : "BID";
      legs.push({
        bucket: row.name, instrument: inst.instrument, market: inst.market,
        side, qty, price, notional: slotNotional, deltaPct: row.deviationPct,
      });
    }
  }

  return { seq: cycle, t: now, portfolioValue, bucketRows, legs };
}

function jitter(current: number, vol: number): number {
  const u1 = Math.max(1e-9, Math.random());
  const u2 = Math.random();
  const z = Math.sqrt(-2 * Math.log(u1)) * Math.cos(2 * Math.PI * u2);
  const next = current * (1 + vol * z);
  return next > 0 ? next : current * 0.5;
}
