// In-memory cross-venue arbitrage / spread-capture simulation.
//
// The scanner watches a set of Canton (and a couple of CEX) venues for the same
// trading pair. Whenever the best available buy price on one venue and the best
// sell price on another diverge, that gap is a spread opportunity (in bps). When
// the gap clears the configured threshold the agent would "act" (route a
// buy-low / sell-high pair of orders) — here nothing real is sent.
//
// This mirrors the shape of crates/agent-arbitrage but is a self-contained,
// synthetic animation: per-pair bps values mean-revert around hand-tuned anchors
// (see demo-data.ts) and every scan cycle emits believable spread rows plus a
// running profitability aggregation.

import {
  BASE_BPS,
  CANTON_VENUES,
  CHART_BPS_CURVES,
  CHART_PAIR_KEYS,
  decimalsFor,
  demoProfitability,
  demoSupplementalSpreads,
  venueName,
  type ChartPairKey,
  type PairCurve,
  type Profitability,
  type SpreadDto,
} from "./demo-data";
import { nextBps, type WalkParams } from "./spread-simulator";

export type ArbitrageConfig = Readonly<{
  focusPair: ChartPairKey; // pair plotted on the spread chart
  minSpreadBps: number; // act threshold — rows at/above this are executed
  tradeSizeUsd: number; // notional per opportunity (drives est. profit)
  scanIntervalSecs: number; // seconds between scan cycles
}>;

export type SpreadPoint = Readonly<{ t: number; bps: number; acted: boolean }>;

export interface VenueStat {
  venueId: string;
  count: number;
  buyCount: number;
  sellCount: number;
  profitUsd: number;
}

export interface SizeBucket {
  label: string;
  min: number;
  max: number | null;
  count: number;
}

export interface ArbitrageState {
  config: ArbitrageConfig;
  status: "scanning" | "idle";
  pairBps: Record<string, number>; // live bps per pair
  series: SpreadPoint[]; // focus-pair bps history (chart)
  recent: SpreadDto[]; // most-recent-first spread rows
  scans: number;
  spreadsFound: number;
  actedCount: number;
  estProfitUsd: number; // Σ est. profit across every found opportunity
  realizedUsd: number; // Σ est. profit across acted opportunities
  bestBps: number;
  avgBps: number;
  bpsSamples: number;
  lastScanAt: number;
  byVenue: VenueStat[];
  bySize: SizeBucket[];
  rotor: number; // rotating index over pairs for scan selection
  nextId: number;
}

const MAX_SERIES = 300;
const MAX_RECENT = 40;
const ROWS_PER_SCAN = 2;

export function initState(config: ArbitrageConfig): ArbitrageState {
  const pairBps: Record<string, number> = {};
  for (const pair of CHART_PAIR_KEYS) pairBps[pair] = BASE_BPS[pair];

  const prof: Profitability = demoProfitability();
  const byVenue: VenueStat[] = prof.byVenue.map((v) => ({
    venueId: v.venueId,
    count: v.count,
    buyCount: v.buyCount,
    sellCount: v.sellCount,
    profitUsd: Number(v.estProfitUsd),
  }));
  const bySize: SizeBucket[] = prof.bySize.map((b) => ({ ...b }));

  return {
    config,
    status: "scanning",
    pairBps,
    series: [{ t: Date.now(), bps: pairBps[config.focusPair]!, acted: false }],
    recent: demoSupplementalSpreads().slice(0, MAX_RECENT),
    scans: 0,
    spreadsFound: prof.totals.count,
    actedCount: 0,
    estProfitUsd: Number(prof.totals.estProfitUsd),
    realizedUsd: 0,
    bestBps: prof.totals.maxBps,
    avgBps: prof.totals.avgBps,
    bpsSamples: prof.totals.count,
    lastScanAt: 0,
    byVenue,
    bySize,
    rotor: 0,
    nextId: 1,
  };
}

/**
 * Advances one tick: walk every pair's bps, and — when a scan cycle is due —
 * emit fresh spread rows and fold them into the running aggregation.
 */
export function step(
  state: ArbitrageState,
  walk: WalkParams,
  now: number,
  focusOverride?: number | null,
): { state: ArbitrageState; events: string[] } {
  if (state.status !== "scanning") return { state, events: [] };
  const events: string[] = [];

  // 1. Walk every pair's live spread.
  for (const pair of CHART_PAIR_KEYS) {
    const override = pair === state.config.focusPair ? focusOverride : undefined;
    state.pairBps[pair] = nextBps(state.pairBps[pair]!, BASE_BPS[pair], walk, override);
  }
  const focusBps = state.pairBps[state.config.focusPair]!;

  // 2. Scan cycle due?
  let tickActed = false;
  const dueAt = state.lastScanAt + state.config.scanIntervalSecs * 1000;
  if (state.lastScanAt === 0 || now >= dueAt) {
    const picks = pickPairs(state);
    for (const pair of picks) {
      const row = buildRow(state, pair, now);
      state.recent.unshift(row);
      if (state.recent.length > MAX_RECENT) state.recent.pop();
      foldRow(state, row);
      if (row.acted) {
        tickActed = tickActed || pair === state.config.focusPair;
        events.push(
          `ACT ${pair} ${venueName(row.buyVenueId)} → ${venueName(row.sellVenueId)} ${row.spreadBps} bps ($${row.estProfitUsd})`,
        );
      } else {
        events.push(
          `SCAN ${pair} ${venueName(row.buyVenueId)} → ${venueName(row.sellVenueId)} ${row.spreadBps} bps (below ${state.config.minSpreadBps})`,
        );
      }
    }
    state.scans += 1;
    state.lastScanAt = now;
  }

  // 3. Record the focus-pair point for the chart.
  state.series.push({ t: now, bps: focusBps, acted: tickActed });
  if (state.series.length > MAX_SERIES) state.series.shift();

  return { state, events };
}

function pickPairs(state: ArbitrageState): ChartPairKey[] {
  const picks: ChartPairKey[] = [];
  for (let k = 0; k < ROWS_PER_SCAN; k++) {
    picks.push(CHART_PAIR_KEYS[state.rotor % CHART_PAIR_KEYS.length]!);
    state.rotor += 1;
  }
  // Bias toward keeping the focused pair visible in the feed.
  if (!picks.includes(state.config.focusPair) && Math.random() < 0.5) {
    picks[0] = state.config.focusPair;
  }
  return picks;
}

function routeFor(spec: PairCurve): [string, string] {
  // Half the time use the pair's canonical route; otherwise pick a random
  // distinct pair of Canton venues for variety.
  if (Math.random() < 0.5) return [spec.buyVenue, spec.sellVenue];
  const pool = CANTON_VENUES;
  const buy = pool[Math.floor(Math.random() * pool.length)]!;
  let sell = pool[Math.floor(Math.random() * pool.length)]!;
  if (sell === buy) sell = pool[(pool.indexOf(buy as (typeof CANTON_VENUES)[number]) + 1) % pool.length]!;
  return [buy, sell];
}

function buildRow(state: ArbitrageState, pair: ChartPairKey, now: number): SpreadDto {
  const spec = CHART_BPS_CURVES[pair];
  const [buy, sell] = routeFor(spec);
  const bps = Math.round(state.pairBps[pair]!);
  const buyPrice = Number(spec.buyPrice);
  const sellPrice = buyPrice * (1 + bps / 10_000);
  const estProfit = (state.config.tradeSizeUsd * bps) / 10_000;
  return {
    id: `live-${state.nextId++}`,
    ts: new Date(now).toISOString(),
    basePairKey: pair,
    buyVenueId: buy,
    sellVenueId: sell,
    buyPrice: spec.buyPrice,
    sellPrice: sellPrice.toFixed(decimalsFor(buyPrice)),
    spreadBps: bps,
    estProfitUsd: estProfit.toFixed(4),
    acted: bps >= state.config.minSpreadBps,
  };
}

function foldRow(state: ArbitrageState, row: SpreadDto): void {
  const profit = Number(row.estProfitUsd);
  state.spreadsFound += 1;
  state.estProfitUsd += profit;
  if (row.spreadBps > state.bestBps) state.bestBps = row.spreadBps;
  state.bpsSamples += 1;
  state.avgBps = state.avgBps + (row.spreadBps - state.avgBps) / state.bpsSamples;
  if (row.acted) {
    state.actedCount += 1;
    state.realizedUsd += profit;
  }
  bumpVenue(state, row.buyVenueId, "buy", profit);
  bumpVenue(state, row.sellVenueId, "sell", profit);
  bumpSize(state, row.spreadBps);
}

function bumpVenue(state: ArbitrageState, venueId: string, side: "buy" | "sell", profit: number): void {
  let v = state.byVenue.find((x) => x.venueId === venueId);
  if (!v) {
    v = { venueId, count: 0, buyCount: 0, sellCount: 0, profitUsd: 0 };
    state.byVenue.push(v);
  }
  v.count += 1;
  if (side === "buy") v.buyCount += 1;
  else v.sellCount += 1;
  v.profitUsd += profit / 2; // profit attributed across both legs
}

function bumpSize(state: ArbitrageState, bps: number): void {
  const b = state.bySize.find((x) => bps >= x.min && (x.max === null || bps < x.max));
  if (b) b.count += 1;
}
