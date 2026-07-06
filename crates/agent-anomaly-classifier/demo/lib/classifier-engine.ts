// Port of agent-anomaly-classifier. Streams synthetic anomaly records
// and clusters each into spoofing / wash_trading / stuck_settlement /
// normal_volatility via a rule-based co-occurrence classifier.

export type SourceKind = "anomaly.layer_cluster" | "anomaly.rapid_cancel" | "anomaly.fill_before_cancel_burst" | "anomaly.stuck_settlement";
export type ClassLabel = "spoofing" | "wash_trading" | "stuck_settlement" | "normal_volatility";

export type Anomaly = Readonly<{
  seq: number;
  t: number;
  kind: SourceKind;
  market: string;
  detail: string;
  classLabel: ClassLabel;
  windowSize: number;
}>;

export type ClassifierConfig = Readonly<{
  windowSecs: number;
  clusterThreshold: number;
  markets: string[];
  ratePerSec: number;
  injectSpoofing: boolean;
  injectWashTrading: boolean;
}>;

export type ClassifierState = {
  config: ClassifierConfig;
  status: "running" | "idle";
  anomalies: Anomaly[];          // ~60
  totalSeen: number;
  byClass: Record<string, number>;
  bySourceKind: Record<string, number>;
  byMarket: Record<string, number>;
  windows: Record<string, Array<{ t: number; kind: SourceKind }>>;
  nextSpawnAt: number;
  nextSeq: number;
};

const MAX_ANOMALIES = 60;
const SOURCE_KINDS: SourceKind[] = ["anomaly.layer_cluster", "anomaly.rapid_cancel", "anomaly.fill_before_cancel_burst", "anomaly.stuck_settlement"];

export function initState(config: ClassifierConfig, now: number): ClassifierState {
  return {
    config, status: "running",
    anomalies: [], totalSeen: 0,
    byClass: {}, bySourceKind: {}, byMarket: {},
    windows: {}, nextSpawnAt: now, nextSeq: 1,
  };
}

export function step(state: ClassifierState, _price: number, now: number): { state: ClassifierState; log: string[] } {
  if (state.status !== "running") return { state, log: [] };
  const log: string[] = [];
  const interval = 1000 / Math.max(0.1, state.config.ratePerSec);
  while (now >= state.nextSpawnAt) {
    ingest(state, spawn(state, state.nextSpawnAt), log);
    state.nextSpawnAt += interval;
  }
  return { state, log };
}

function spawn(state: ClassifierState, t: number): { kind: SourceKind; market: string; detail: string } {
  const market = state.config.markets[Math.floor(Math.random() * state.config.markets.length)];
  // Optional spoofing injection: choose a "hot" market and emit a rapid succession.
  if (state.config.injectSpoofing && Math.random() < 0.35) {
    const hot = state.config.markets[0];
    const kind = Math.random() < 0.4 ? "anomaly.layer_cluster" : Math.random() < 0.5 ? "anomaly.rapid_cancel" : "anomaly.fill_before_cancel_burst";
    return { kind: kind as SourceKind, market: hot, detail: injectedDetail(kind as SourceKind) };
  }
  if (state.config.injectWashTrading && Math.random() < 0.25) {
    // wash-trading = many rapid cancels on one market, no layer
    return { kind: "anomaly.rapid_cancel", market: state.config.markets[state.config.markets.length - 1], detail: "create+cancel 220ms" };
  }
  if (Math.random() < 0.08) {
    return { kind: "anomaly.stuck_settlement", market, detail: `age ${900 + Math.floor(Math.random() * 3600)}s` };
  }
  const kind = SOURCE_KINDS[Math.floor(Math.random() * SOURCE_KINDS.length)];
  return { kind, market, detail: injectedDetail(kind) };
}

function injectedDetail(k: SourceKind): string {
  switch (k) {
    case "anomaly.layer_cluster": return `${3 + Math.floor(Math.random() * 4)} orders in band`;
    case "anomaly.rapid_cancel": return `create+cancel ${100 + Math.floor(Math.random() * 800)}ms`;
    case "anomaly.fill_before_cancel_burst": return `${3 + Math.floor(Math.random() * 6)} cancels`;
    case "anomaly.stuck_settlement": return `age ${900 + Math.floor(Math.random() * 3600)}s`;
  }
}

function ingest(state: ClassifierState, ev: { kind: SourceKind; market: string; detail: string }, log: string[]) {
  const seq = state.nextSeq++;
  const t = Date.now();
  const window = state.windows[ev.market] ?? [];
  const cutoff = t - state.config.windowSecs * 1000;
  while (window.length > 0 && window[0].t < cutoff) window.shift();
  window.push({ t, kind: ev.kind });
  state.windows[ev.market] = window;

  const classLabel = classify(ev.kind, window.map((w) => w.kind), state.config.clusterThreshold);
  const rec: Anomaly = { seq, t, kind: ev.kind, market: ev.market, detail: ev.detail, classLabel, windowSize: window.length };
  state.anomalies.push(rec);
  while (state.anomalies.length > MAX_ANOMALIES) state.anomalies.shift();
  state.totalSeen += 1;
  state.byClass[classLabel] = (state.byClass[classLabel] ?? 0) + 1;
  state.bySourceKind[ev.kind] = (state.bySourceKind[ev.kind] ?? 0) + 1;
  state.byMarket[ev.market] = (state.byMarket[ev.market] ?? 0) + 1;
  log.push(`${classLabel.padEnd(18)} #${seq}  ${ev.kind.replace("anomaly.", "").padEnd(24)}  ${ev.market.padEnd(10)}  win=${window.length}`);
}

function classify(kind: SourceKind, windowKinds: SourceKind[], clusterThreshold: number): ClassLabel {
  if (kind === "anomaly.stuck_settlement") return "stuck_settlement";
  const nLayer = windowKinds.filter((k) => k === "anomaly.layer_cluster").length;
  const nRapid = windowKinds.filter((k) => k === "anomaly.rapid_cancel").length;
  const nBurst = windowKinds.filter((k) => k === "anomaly.fill_before_cancel_burst").length;
  if (nLayer >= 1 && (nRapid + nBurst) >= clusterThreshold) return "spoofing";
  if (nRapid + nBurst >= clusterThreshold * 2) return "spoofing";
  if (windowKinds.length >= clusterThreshold * 2 && nLayer === 0) return "wash_trading";
  return "normal_volatility";
}

export function classLabelColor(c: ClassLabel): string {
  switch (c) {
    case "spoofing": return "var(--neg)";
    case "wash_trading": return "#ffd66b";
    case "stuck_settlement": return "var(--accent)";
    case "normal_volatility": return "var(--pos)";
  }
}
