// Port of agent-inventory-risk to TypeScript. Mirrors
// crates/agent-inventory-risk/src/main.rs (fn inv_risk_loop) — every cycle:
//  1. Read own unlocked balance of `instrument`.
//  2. Compute delta = balance - target.
//  3. Classify zone:
//     - |delta| <= soft_tolerance   → "ok"
//     - <= hard_tolerance           → "soft_band" (emit inventory.risk signal, no order)
//     - >  hard_tolerance           → "hard_band" (auto-hedge if enabled — place
//       BID/OFFER to bring balance back toward target)
//
// The demo simulates a walking balance driven by the market mid and by a
// background "trading strategy" that pushes exposure around. Whenever a
// hedge order fires, it starts filling over the next few ticks, gradually
// nudging the balance toward the target — you can visibly see the band
// snap the position back.

export type Zone = "ok" | "soft_band" | "hard_band";

export type InvRiskConfig = Readonly<{
  instrument: string;
  hedgeMarket: string;
  target: number;                       // target balance
  softTolerance: number;                // > 0
  hardTolerance: number;                // > softTolerance
  startingBalance: number;
  startingPrice: number;
  autoHedge: boolean;
  checkIntervalSecs: number;            // >= 1
  driftPerCycle: number;                // exposure drift each cycle (positive = accumulating)
}>;

export type HedgeOrder = {
  id: number;
  side: "BID" | "OFFER";
  targetPrice: number;
  originalQty: number;
  remaining: number;
  placedAt: number;
};

export type SignalRecord = Readonly<{
  seq: number;
  t: number;
  zone: Zone;
  balance: number;
  delta: number;
  suggestedSide: "BID" | "OFFER";
  suggestedQty: number;
  hedgeFired: boolean;
}>;

export type InvRiskState = {
  config: InvRiskConfig;
  status: "running" | "idle";
  balance: number;                     // walking
  currentPrice: number;                // walking
  cycle: number;
  currentZone: Zone;
  softSignalsEmitted: number;
  hardBreaches: number;
  hedgesPlaced: number;
  hedgesFilled: number;
  totalHedgeNotional: number;
  activeHedges: HedgeOrder[];
  recentSignals: SignalRecord[];       // ~30
  balanceHistory: Array<{ t: number; balance: number }>;
  nextCheckAt: number;
  nextHedgeId: number;
};

const MAX_SIGNALS = 30;
const MAX_HISTORY = 240;

export function initState(config: InvRiskConfig, now: number): InvRiskState {
  return {
    config,
    status: "running",
    balance: config.startingBalance,
    currentPrice: config.startingPrice,
    cycle: 0,
    currentZone: classify(config.startingBalance - config.target, config.softTolerance, config.hardTolerance),
    softSignalsEmitted: 0,
    hardBreaches: 0,
    hedgesPlaced: 0,
    hedgesFilled: 0,
    totalHedgeNotional: 0,
    activeHedges: [],
    recentSignals: [],
    balanceHistory: [{ t: now, balance: config.startingBalance }],
    nextCheckAt: now + config.checkIntervalSecs * 1000,
    nextHedgeId: 1,
  };
}

/** Advance one tick. `price` walks the hedge-market mid. */
export function step(state: InvRiskState, price: number, now: number): { state: InvRiskState; log: string[] } {
  if (state.status !== "running") return { state, log: [] };
  const log: string[] = [];
  state.currentPrice = price;

  // 1. Trading-strategy pressure — pushes balance around each tick.
  //    Base drift + jitter proportional to soft tolerance so scale feels right.
  const wobble = (Math.random() - 0.5) * state.config.softTolerance * 0.3;
  state.balance += state.config.driftPerCycle / 4 + wobble;

  // 2. Fill active hedge orders gradually. ~20% remaining fills per tick.
  const survivors: HedgeOrder[] = [];
  for (const h of state.activeHedges) {
    const fill = Math.min(h.remaining, Math.max(h.remaining * 0.2, 0.01));
    // BID hedge brings balance UP; OFFER brings it DOWN.
    if (h.side === "BID") state.balance += fill;
    else state.balance -= fill;
    h.remaining -= fill;
    if (h.remaining > 0.001) {
      survivors.push(h);
    } else {
      state.hedgesFilled += 1;
      log.push(`FILL  hedge id=${h.id} ${h.side} qty=${h.originalQty.toFixed(3)} on ${state.config.hedgeMarket}`);
    }
  }
  state.activeHedges = survivors;

  // 3. Snapshot balance history each tick for chart.
  state.balanceHistory.push({ t: now, balance: state.balance });
  while (state.balanceHistory.length > MAX_HISTORY) state.balanceHistory.shift();

  // 4. Periodic check.
  if (now >= state.nextCheckAt) {
    state.cycle += 1;
    const delta = state.balance - state.config.target;
    const zone = classify(delta, state.config.softTolerance, state.config.hardTolerance);
    state.currentZone = zone;

    const side: "BID" | "OFFER" = delta > 0 ? "OFFER" : "BID";
    const qty = Math.abs(delta);

    let hedgeFired = false;
    if (zone === "hard_band" && state.config.autoHedge) {
      // Place hedge order sized to bring balance back to target
      // (avoid stacking: skip if we already have an active hedge on this side)
      const alreadyActive = state.activeHedges.some((h) => h.side === side);
      if (!alreadyActive && qty > 0) {
        const h: HedgeOrder = {
          id: state.nextHedgeId++,
          side,
          targetPrice: state.currentPrice,
          originalQty: qty,
          remaining: qty,
          placedAt: now,
        };
        state.activeHedges.push(h);
        state.hedgesPlaced += 1;
        state.totalHedgeNotional += qty * state.currentPrice;
        hedgeFired = true;
        state.hardBreaches += 1;
        log.push(`HARD  hedge id=${h.id} ${side} qty=${qty.toFixed(3)} @ ${state.currentPrice.toFixed(6)} on ${state.config.hedgeMarket}`);
      } else if (alreadyActive) {
        log.push(`HARD  already-hedging (${side}) — skipping stack`);
        state.hardBreaches += 1;
      }
    } else if (zone === "hard_band") {
      state.hardBreaches += 1;
      log.push(`HARD  band delta=${delta.toFixed(3)} — auto-hedge disabled`);
    } else if (zone === "soft_band") {
      state.softSignalsEmitted += 1;
      log.push(`SOFT  signal delta=${delta.toFixed(3)} suggest ${side} ${qty.toFixed(3)}`);
    } else {
      log.push(`OK    delta=${delta.toFixed(3)}`);
    }

    const rec: SignalRecord = {
      seq: state.cycle, t: now, zone, balance: state.balance,
      delta, suggestedSide: side, suggestedQty: qty, hedgeFired,
    };
    state.recentSignals.push(rec);
    while (state.recentSignals.length > MAX_SIGNALS) state.recentSignals.shift();
    state.nextCheckAt = now + state.config.checkIntervalSecs * 1000;
  }

  return { state, log };
}

function classify(delta: number, soft: number, hard: number): Zone {
  const a = Math.abs(delta);
  if (a <= soft) return "ok";
  if (a <= hard) return "soft_band";
  return "hard_band";
}
