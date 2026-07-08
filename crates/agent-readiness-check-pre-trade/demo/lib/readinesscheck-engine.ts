// Port of agent-readiness-check-pre-trade pre-trade gate logic to TypeScript.
// Mirrors crates/agent-readiness-check-pre-trade/src/main.rs (fn check).
//
// One-shot / periodic sanity check that verifies:
//   - required per-instrument balances are >= minAmount
//   - failed settlement count is <= maxFailedSettlements
//   - pending settlement count is <= maxPendingSettlements
//   - if requirePreapproval, at least one TransferPreapproval exists
//
// The demo runs the check every checkIntervalSecs against a synthetic party
// state that drifts over time so operators can watch READY / NOT READY
// transitions in real time.

export type RequiredBalance = Readonly<{ instrument: string; minAmount: number }>;

export type ReadinessCheckConfig = Readonly<{
  requiredBalances: RequiredBalance[];
  maxFailedSettlements: number;
  maxPendingSettlements: number;
  requirePreapproval: boolean;
  checkIntervalSecs: number;
  startingPrice: number;
  initialBalances: { instrument: string; balance: number }[];
  initialFailed: number;
  initialPending: number;
  initialPreapproval: boolean;
  driftEnabled: boolean;
}>;

export type CheckResult = Readonly<{
  name: string;
  ok: boolean;
  current?: number | string;
  threshold?: number | string;
  message: string;
}>;

export type ReadinessCheckState = {
  config: ReadinessCheckConfig;
  status: "monitoring" | "idle";
  currentPrice: number;
  currentBalances: { instrument: string; balance: number }[];
  failedSettlements: number;
  pendingSettlements: number;
  preapproval: boolean;
  checks: CheckResult[];
  overallReady: boolean;
  checksCount: number;
  lastCheckAt: number | null;
  transitionsCount: number;
  samples: number;
};

export function initState(config: ReadinessCheckConfig, startPrice: number): ReadinessCheckState {
  // Merge required-balances with any extra initialBalances by instrument.
  const balMap = new Map<string, number>();
  for (const rb of config.requiredBalances) balMap.set(rb.instrument, 0);
  for (const b of config.initialBalances) balMap.set(b.instrument, b.balance);
  return {
    config,
    status: "monitoring",
    currentPrice: startPrice,
    currentBalances: Array.from(balMap.entries()).map(([instrument, balance]) => ({ instrument, balance })),
    failedSettlements: config.initialFailed,
    pendingSettlements: config.initialPending,
    preapproval: config.initialPreapproval,
    checks: [],
    overallReady: false,
    checksCount: 0,
    lastCheckAt: null,
    transitionsCount: 0,
    samples: 0,
  };
}

/** Runs one tick: drifts synthetic state and re-evaluates checks on cadence. */
export function step(state: ReadinessCheckState, price: number, now: number): { state: ReadinessCheckState; events: string[] } {
  if (state.status !== "monitoring") return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;
  state.samples += 1;

  // Drift synthetic party state
  if (state.config.driftEnabled) {
    for (const b of state.currentBalances) {
      const delta = gauss() * 0.5;
      b.balance = Math.max(0, b.balance + delta);
    }
    // Rare pending increment / decrement
    if (Math.random() < 0.15) {
      state.pendingSettlements = Math.max(0, state.pendingSettlements + (Math.random() < 0.55 ? -1 : 1));
    }
    // Rare failed increment (mostly stays stable)
    if (Math.random() < 0.04) {
      state.failedSettlements = state.failedSettlements + 1;
    }
    // Very rare preapproval flip
    if (state.config.requirePreapproval && Math.random() < 0.01) {
      state.preapproval = !state.preapproval;
    }
  }

  // Only actually re-run checks on cadence
  const dueAt = (state.lastCheckAt ?? 0) + state.config.checkIntervalSecs * 1000;
  if (state.lastCheckAt !== null && now < dueAt) {
    return { state, events };
  }

  const prevReady = state.overallReady;
  const checks: CheckResult[] = [];

  // Balance checks
  for (const rb of state.config.requiredBalances) {
    const have = state.currentBalances.find((b) => b.instrument === rb.instrument)?.balance ?? 0;
    const ok = have >= rb.minAmount;
    checks.push({
      name: `balance ${rb.instrument}`,
      ok,
      current: round4(have),
      threshold: `>= ${rb.minAmount}`,
      message: ok
        ? `${rb.instrument} balance ${have.toFixed(3)} >= ${rb.minAmount}`
        : `${rb.instrument} balance ${have.toFixed(3)} < required ${rb.minAmount}`,
    });
  }

  // Failed settlements
  {
    const ok = state.failedSettlements <= state.config.maxFailedSettlements;
    checks.push({
      name: "failed settlements",
      ok,
      current: state.failedSettlements,
      threshold: `<= ${state.config.maxFailedSettlements}`,
      message: ok
        ? `failed=${state.failedSettlements} <= ${state.config.maxFailedSettlements}`
        : `failed=${state.failedSettlements} > ${state.config.maxFailedSettlements}`,
    });
  }

  // Pending settlements
  {
    const ok = state.pendingSettlements <= state.config.maxPendingSettlements;
    checks.push({
      name: "pending settlements",
      ok,
      current: state.pendingSettlements,
      threshold: `<= ${state.config.maxPendingSettlements}`,
      message: ok
        ? `pending=${state.pendingSettlements} <= ${state.config.maxPendingSettlements}`
        : `pending=${state.pendingSettlements} > ${state.config.maxPendingSettlements}`,
    });
  }

  // Preapproval
  if (state.config.requirePreapproval) {
    const ok = state.preapproval;
    checks.push({
      name: "TransferPreapproval",
      ok,
      current: ok ? "present" : "missing",
      threshold: "present",
      message: ok ? "TransferPreapproval present" : "no active TransferPreapproval",
    });
  }

  const ready = checks.every((c) => c.ok);
  state.checks = checks;
  state.overallReady = ready;
  state.checksCount += 1;
  state.lastCheckAt = now;
  if (state.checksCount > 1 && prevReady !== ready) state.transitionsCount += 1;

  if (ready) {
    events.push(`CHECK #${state.checksCount}: ready (${checks.length} checks passed)`);
  } else {
    const failed = checks.filter((c) => !c.ok).map((c) => c.name).join(", ");
    events.push(`CHECK #${state.checksCount}: FAIL — ${failed}`);
  }
  return { state, events };
}

function gauss(): number {
  // Box-Muller — one N(0,1) sample.
  let u = 0, v = 0;
  while (u === 0) u = Math.random();
  while (v === 0) v = Math.random();
  return Math.sqrt(-2.0 * Math.log(u)) * Math.cos(2.0 * Math.PI * v);
}

function round4(n: number): number {
  return Math.round(n * 10000) / 10000;
}
