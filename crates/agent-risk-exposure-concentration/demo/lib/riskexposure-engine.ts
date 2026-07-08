// Port of agent-risk-exposure-concentration snapshot logic to TypeScript. Mirrors
// crates/agent-risk-exposure-concentration/src/main.rs (fn snapshot_once).
//
// Rule (per snapshot cycle):
//   priceQuote_i = mid × priceMultiplier_i
//   valueQuote_i = balance_i × priceQuote_i
//   sharePct_i   = valueQuote_i / sum(valueQuote) × 100
//   concentrationWarn = any(sharePct_i > concentrationWarnPct)
//
// Read-only: no orders, no writes. Mock open notional + pending settlements
// drift slowly to make the dashboard feel alive.

export type Instrument = Readonly<{
  name: string;
  balance: number;
  priceMultiplier: number; // mock price = mid × this
}>;

export type RiskExposureConfig = Readonly<{
  markets: string[];
  instruments: Instrument[];
  concentrationWarnPct: number; // default 60
  snapshotIntervalSecs: number; // default 10
  startingPrice: number;        // seed for the price simulator
}>;

export type Position = Readonly<{
  instrument: string;
  balance: number;
  priceQuote: number;
  valueQuote: number;
  sharePct: number;
}>;

export type OpenNotional = Readonly<{
  market: string;
  notional: number;
}>;

export type RiskExposureState = {
  config: RiskExposureConfig;
  status: "monitoring" | "idle";
  currentPrice: number;
  startingPortfolioQuote: number;
  positions: Position[];
  totalPortfolioQuote: number;
  openNotionalPerMarket: OpenNotional[];
  pendingSettlementsNotional: number;
  concentrationWarn: boolean;
  warnedInstrument: string | null;
  snapshotsCount: number;
  ticksSinceLastSnapshot: number;
  lastSnapshotAt: number | null;
};

export function initState(config: RiskExposureConfig, startPrice: number): RiskExposureState {
  const positions = computePositions(config.instruments, startPrice);
  const total = positions.reduce((s, p) => s + p.valueQuote, 0);
  const withShare = positions.map((p) => ({
    ...p,
    sharePct: total > 0 ? (p.valueQuote / total) * 100 : 0,
  }));
  const openNotional = config.markets.map((m, i) => ({
    market: m,
    notional: total > 0 ? total * (0.02 + 0.01 * (i % 3)) : 0,
  }));
  return {
    config,
    status: "monitoring",
    currentPrice: startPrice,
    startingPortfolioQuote: total,
    positions: withShare,
    totalPortfolioQuote: total,
    openNotionalPerMarket: openNotional,
    pendingSettlementsNotional: total * 0.015,
    concentrationWarn: withShare.some((p) => p.sharePct > config.concentrationWarnPct),
    warnedInstrument: pickTop(withShare),
    snapshotsCount: 0,
    ticksSinceLastSnapshot: 0,
    lastSnapshotAt: null,
  };
}

function computePositions(instruments: readonly Instrument[], mid: number): Position[] {
  return instruments.map((i) => {
    const priceQuote = mid * i.priceMultiplier;
    const valueQuote = i.balance * priceQuote;
    return {
      instrument: i.name,
      balance: i.balance,
      priceQuote,
      valueQuote,
      sharePct: 0,
    };
  });
}

function pickTop(positions: readonly Position[]): string | null {
  let top: Position | null = null;
  for (const p of positions) {
    if (p.valueQuote > 0 && (top === null || p.sharePct > top.sharePct)) top = p;
  }
  return top ? top.instrument : null;
}

/** Applies a tick. Returns state + emitted events. */
export function step(state: RiskExposureState, price: number, now: number): { state: RiskExposureState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;
  state.ticksSinceLastSnapshot += 1;

  const intervalTicks = Math.max(1, Math.round(state.config.snapshotIntervalSecs));
  if (state.ticksSinceLastSnapshot < intervalTicks && state.snapshotsCount > 0) {
    // Only recompute the on-screen positions so the chart keeps updating,
    // but don't emit a snapshot event.
    recomputeLive(state, price);
    return { state, events };
  }

  // Emit a snapshot: recompute + drift the mock exposures + evaluate warn.
  recomputeLive(state, price);
  driftExposures(state, now);
  const prevWarn = state.concentrationWarn;
  state.concentrationWarn = state.positions.some((p) => p.sharePct > state.config.concentrationWarnPct);
  state.warnedInstrument = pickTop(state.positions);
  state.snapshotsCount += 1;
  state.lastSnapshotAt = now;
  state.ticksSinceLastSnapshot = 0;

  const top = state.positions.reduce<Position | null>((acc, p) => (acc === null || p.sharePct > acc.sharePct ? p : acc), null);
  events.push(
    `SNAPSHOT #${state.snapshotsCount} portfolio=${fmt(state.totalPortfolioQuote)} top=${top?.instrument ?? "—"} ${(top?.sharePct ?? 0).toFixed(2)}%`,
  );

  if (state.concentrationWarn && !prevWarn && top) {
    events.push(
      `WARN: ${top.instrument} concentration ${top.sharePct.toFixed(2)}pct > ${state.config.concentrationWarnPct}pct`,
    );
  } else if (!state.concentrationWarn && prevWarn) {
    events.push(`OK: concentration cleared (below ${state.config.concentrationWarnPct}%)`);
  }

  return { state, events };
}

function recomputeLive(state: RiskExposureState, mid: number) {
  const positions = computePositions(state.config.instruments, mid);
  const total = positions.reduce((s, p) => s + p.valueQuote, 0);
  state.positions = positions.map((p) => ({
    ...p,
    sharePct: total > 0 ? (p.valueQuote / total) * 100 : 0,
  }));
  state.totalPortfolioQuote = total;
}

// Slow random drift for open notional per market + pending settlement notional.
function driftExposures(state: RiskExposureState, now: number) {
  const total = state.totalPortfolioQuote;
  const seed = state.snapshotsCount + Math.floor(now / 1000);
  state.openNotionalPerMarket = state.config.markets.map((m, i) => {
    const wobble = 0.9 + 0.2 * pseudoRandom(seed + i);
    const base = 0.02 + 0.01 * (i % 3);
    return { market: m, notional: Math.max(0, total * base * wobble) };
  });
  const pendingWobble = 0.85 + 0.3 * pseudoRandom(seed + 999);
  state.pendingSettlementsNotional = Math.max(0, total * 0.015 * pendingWobble);
}

function pseudoRandom(n: number): number {
  const x = Math.sin(n * 12.9898) * 43758.5453;
  return x - Math.floor(x);
}

function fmt(n: number): string {
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 6;
  return n.toFixed(digits);
}
