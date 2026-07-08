// Port of agent-risk-alert-passive threshold-monitor logic to TypeScript. Mirrors
// crates/agent-risk-alert-passive/src/main.rs.
//
// This is a PASSIVE monitor: it polls state and emits an alert whenever any
// configured threshold is breached. It does NOT cancel orders or trade.
// Thresholds:
//   - maxOpenOrders          — total active orders
//   - maxFailedSettlements   — failed settlement proposals
//   - maxOpenNotional        — sum of price × remaining_qty across open orders

export type AlertKind = "open-orders" | "failed-settlements" | "open-notional";

export type RiskAlertConfig = Readonly<{
  maxOpenOrders: number;         // e.g. 200
  maxFailedSettlements: number;  // e.g. 3
  maxOpenNotional: number;       // e.g. 100000
  checkIntervalSecs: number;     // e.g. 5
  orderGrowthPerTick: number;    // fractional accumulator, e.g. 2 (orders/sec)
  notionalGrowthPerTick: number; // per-second growth, e.g. 500
  failureRatePerTick: number;    // probability [0,1] a failure occurs per second
  startingPrice: number;         // for the mid readout
}>;

export type Alert = Readonly<{
  seq: number;
  t: number;
  kind: AlertKind;
  value: number;
  threshold: number;
  cleared: boolean;   // false = breach, true = OK / cleared
  message: string;
}>;

export type RiskAlertState = {
  config: RiskAlertConfig;
  status: "monitoring" | "idle";
  currentPrice: number;
  openOrders: number;         // integer (running counter)
  ordersAccum: number;        // fractional accumulator
  failedSettlements: number;
  openNotional: number;
  alerts: Alert[];            // bounded ring
  alertsCount: number;        // total emitted (breach events only)
  lastCheckAt: number | null; // ms
  nextCheckAt: number | null; // ms
  activeBreaches: AlertKind[];
  lastAlertKind: AlertKind | null;
  lastAlertValue: number | null;
};

const MAX_ALERTS = 30;

export function initState(config: RiskAlertConfig): RiskAlertState {
  return {
    config,
    status: "monitoring",
    currentPrice: config.startingPrice,
    openOrders: 0,
    ordersAccum: 0,
    failedSettlements: 0,
    openNotional: 0,
    alerts: [],
    alertsCount: 0,
    lastCheckAt: null,
    nextCheckAt: null,
    activeBreaches: [],
    lastAlertKind: null,
    lastAlertValue: null,
  };
}

/**
 * Advance the simulation by one tick (~1 second of wall time).
 *   - grow open-orders, open-notional, occasionally a settlement failure
 *   - every `checkIntervalSecs` re-evaluate thresholds and emit alert / OK events
 */
export function step(
  state: RiskAlertState,
  price: number,
  now: number,
): { state: RiskAlertState; events: string[] } {
  if (state.status !== "monitoring") return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;

  // --- simulate load growth ------------------------------------------------
  state.ordersAccum += state.config.orderGrowthPerTick;
  if (state.ordersAccum >= 1) {
    const bump = Math.floor(state.ordersAccum);
    state.openOrders += bump;
    state.ordersAccum -= bump;
  }
  const noise = (Math.random() - 0.5) * state.config.notionalGrowthPerTick * 0.6;
  state.openNotional = Math.max(0, state.openNotional + state.config.notionalGrowthPerTick + noise);
  if (state.config.failureRatePerTick > 0 && Math.random() < state.config.failureRatePerTick) {
    state.failedSettlements += 1;
  }

  // --- schedule the periodic threshold check -------------------------------
  const intervalMs = Math.max(1000, state.config.checkIntervalSecs * 1000);
  if (state.nextCheckAt === null) state.nextCheckAt = now + intervalMs;

  if (now < state.nextCheckAt) {
    return { state, events };
  }

  state.lastCheckAt = now;
  state.nextCheckAt = now + intervalMs;

  const checks: Array<{ kind: AlertKind; value: number; threshold: number; label: string }> = [
    { kind: "open-orders", value: state.openOrders, threshold: state.config.maxOpenOrders, label: "open_orders" },
    { kind: "failed-settlements", value: state.failedSettlements, threshold: state.config.maxFailedSettlements, label: "failed_settlements" },
    { kind: "open-notional", value: state.openNotional, threshold: state.config.maxOpenNotional, label: "open_notional" },
  ];

  for (const c of checks) {
    const wasActive = state.activeBreaches.includes(c.kind);
    const nowBreached = c.value > c.threshold;
    if (nowBreached && !wasActive) {
      state.activeBreaches.push(c.kind);
      state.alertsCount += 1;
      const seq = state.alertsCount;
      const msg = `ALERT [${c.kind}] ${c.label} ${fmt(c.value)} > threshold ${fmt(c.threshold)}`;
      const alert: Alert = { seq, t: now, kind: c.kind, value: c.value, threshold: c.threshold, cleared: false, message: msg };
      state.alerts.push(alert);
      if (state.alerts.length > MAX_ALERTS) state.alerts.shift();
      state.lastAlertKind = c.kind;
      state.lastAlertValue = c.value;
      events.push(msg);
    } else if (!nowBreached && wasActive) {
      state.activeBreaches = state.activeBreaches.filter((k) => k !== c.kind);
      const msg = `OK    [${c.kind}] ${c.label} back within limit (${fmt(c.value)} ≤ ${fmt(c.threshold)})`;
      const seq = state.alertsCount + 0; // OKs share the counter — they don't advance it
      const alert: Alert = { seq, t: now, kind: c.kind, value: c.value, threshold: c.threshold, cleared: true, message: msg };
      state.alerts.push(alert);
      if (state.alerts.length > MAX_ALERTS) state.alerts.shift();
      events.push(msg);
    }
  }

  return { state, events };
}

function fmt(n: number): string {
  const abs = Math.abs(n);
  if (abs >= 1000) return n.toFixed(0);
  if (abs >= 1) return n.toFixed(2);
  return n.toFixed(4);
}
