// Port of agent-audit-anomaly-heuristic. Generates a synthetic trading-history
// with injected anomalies + applies four heuristics:
//   - stuck_settlement (fill without settled/failed inside window)
//   - rapid_cancel (create+cancel within window)
//   - layer_cluster (many same-side orders inside a price band)
//   - fill_before_cancel_burst (burst of cancels right after a fill)

export type AnomalyKind = "stuck_settlement" | "rapid_cancel" | "layer_cluster" | "fill_before_cancel_burst";

export type Anomaly = Readonly<{
  seq: number;
  atTs: string;
  kind: AnomalyKind;
  market: string;
  detail: string;
}>;

export type AnomalyConfig = Readonly<{
  historyDays: number;
  recordsPerDay: number;
  markets: string[];
  settlementWindowSecs: number;
  rapidCancelWindowMs: number;
  layerThreshold: number;
  layerBandPct: number;
  burstCount: number;
  burstWindowSecs: number;
  injectAnomalies: boolean;    // inject synthetic anomalies (rapid cancels, layers, stuck settles)
}>;

export type HistoryRec = Readonly<{
  seq: number;
  t: number;
  kind: "order.created" | "order.filled" | "order.cancelled" | "settlement.settled" | "settlement.failed";
  market: string;
  order_id?: number;
  side?: "BID" | "OFFER";
  price?: number;
  proposal_id?: string;
}>;

export type AnomalyState = {
  config: AnomalyConfig;
  history: HistoryRec[];
  anomalies: Anomaly[];
  byKind: Record<string, number>;
  scans: number;
};

const KIND_COLORS: Record<AnomalyKind, string> = {
  stuck_settlement: "var(--neg)",
  rapid_cancel: "var(--accent)",
  layer_cluster: "#ffd66b",
  fill_before_cancel_burst: "var(--neg)",
};
export function anomalyColor(k: AnomalyKind): string { return KIND_COLORS[k]; }

export function initState(config: AnomalyConfig): AnomalyState {
  const history = generate(config);
  return { config, history, anomalies: [], byKind: {}, scans: 0 };
}

export function scan(state: AnomalyState): { state: AnomalyState; log: string[] } {
  const log: string[] = [];
  const c = state.config;
  const anomalies: Anomaly[] = [];
  const orders = new Map<number, HistoryRec>();
  const fills = new Map<string, HistoryRec>();
  const cancelHistory = new Map<string, number[]>();
  const lastFill = new Map<string, number>();
  const openOrders = new Map<string, number[]>();  // key = market|side -> prices

  for (const rec of state.history) {
    const key = `${rec.market}|${rec.side ?? ""}`;
    if (rec.kind === "order.created") {
      if (rec.order_id !== undefined) orders.set(rec.order_id, rec);
      if (rec.price !== undefined && rec.side) {
        if (!openOrders.has(key)) openOrders.set(key, []);
        openOrders.get(key)!.push(rec.price);
        const arr = openOrders.get(key)!;
        if (arr.length >= c.layerThreshold) {
          const mean = arr.reduce((a, b) => a + b, 0) / arr.length;
          const band = mean * c.layerBandPct / 100;
          const inBand = arr.filter((p) => Math.abs(p - mean) <= band).length;
          if (inBand >= c.layerThreshold) {
            anomalies.push({ seq: rec.seq, atTs: new Date(rec.t).toISOString(), kind: "layer_cluster", market: rec.market, detail: `${inBand} orders in band ±${c.layerBandPct}% around mean ${mean.toFixed(4)}` });
          }
        }
      }
    } else if (rec.kind === "order.cancelled") {
      if (rec.order_id !== undefined) {
        const o = orders.get(rec.order_id);
        if (o) {
          orders.delete(rec.order_id);
          const dt = rec.t - o.t;
          if (dt >= 0 && dt < c.rapidCancelWindowMs) {
            anomalies.push({ seq: rec.seq, atTs: new Date(rec.t).toISOString(), kind: "rapid_cancel", market: rec.market, detail: `create seq=${o.seq}, elapsed ${dt}ms` });
          }
          if (o.side && o.price !== undefined) {
            const kk = `${o.market}|${o.side}`;
            const arr = openOrders.get(kk);
            if (arr) {
              const idx = arr.indexOf(o.price);
              if (idx >= 0) arr.splice(idx, 1);
            }
          }
        }
      }
      const arr = cancelHistory.get(rec.market) ?? [];
      arr.push(rec.t);
      const cutoff = rec.t - c.burstWindowSecs * 1000;
      while (arr.length > 0 && arr[0] < cutoff) arr.shift();
      cancelHistory.set(rec.market, arr);
      if (arr.length >= c.burstCount) {
        const last = lastFill.get(rec.market);
        if (last !== undefined) {
          const gap = (arr[0] - last) / 1000;
          if (gap >= 0 && gap <= c.burstWindowSecs) {
            anomalies.push({ seq: rec.seq, atTs: new Date(rec.t).toISOString(), kind: "fill_before_cancel_burst", market: rec.market, detail: `${arr.length} cancels, ${gap.toFixed(1)}s after fill` });
          }
        }
      }
    } else if (rec.kind === "order.filled") {
      lastFill.set(rec.market, rec.t);
      if (rec.proposal_id) fills.set(rec.proposal_id, rec);
    } else if (rec.kind === "settlement.settled" || rec.kind === "settlement.failed") {
      if (rec.proposal_id) fills.delete(rec.proposal_id);
    }
  }

  const now = state.history.length > 0 ? state.history[state.history.length - 1].t : Date.now();
  for (const [, f] of fills) {
    const age = (now - f.t) / 1000;
    if (age >= c.settlementWindowSecs) {
      anomalies.push({ seq: f.seq, atTs: new Date(f.t).toISOString(), kind: "stuck_settlement", market: f.market, detail: `age ${Math.round(age)}s proposal=${f.proposal_id}` });
    }
  }

  const byKind: Record<string, number> = {};
  for (const a of anomalies) byKind[a.kind] = (byKind[a.kind] ?? 0) + 1;

  state.anomalies = anomalies;
  state.byKind = byKind;
  state.scans += 1;
  log.push(`SCAN run #${state.scans}: ${anomalies.length} anomalies in ${state.history.length} records`);
  return { state, log };
}

export function reloadHistory(state: AnomalyState): { state: AnomalyState; log: string[] } {
  state.history = generate(state.config);
  return scan(state);
}

function generate(c: AnomalyConfig): HistoryRec[] {
  const out: HistoryRec[] = [];
  const now = Date.now();
  let seq = 0;
  let nextOrder = 1;
  let nextProposal = 1;
  const open: Array<{ order_id: number; market: string; side: "BID" | "OFFER"; price: number; t: number }> = [];
  const pendingFills: Array<{ proposal_id: string; market: string; t: number }> = [];

  const total = Math.max(20, c.historyDays * c.recordsPerDay);
  for (let i = 0; i < total; i++) {
    const t = now - (total - i) * 30_000 + Math.floor(Math.random() * 20_000);
    const market = c.markets[Math.floor(Math.random() * c.markets.length)];
    // Injected anomalies: 6% rapid cancel, 4% layer burst.
    const injectRapid = c.injectAnomalies && open.length > 0 && Math.random() < 0.06;
    const injectLayer = c.injectAnomalies && Math.random() < 0.04;
    const r = Math.random();
    if (injectRapid) {
      const idx = open.length - 1;
      const o = open[idx];
      open.splice(idx, 1);
      seq += 1;
      out.push({ seq, t: o.t + Math.floor(Math.random() * (c.rapidCancelWindowMs - 100)), kind: "order.cancelled", market: o.market, order_id: o.order_id, side: o.side, price: o.price });
      continue;
    }
    if (injectLayer) {
      // Push several same-side orders in a tight price band on one market.
      const anchor = 100 + Math.random() * 100;
      const side: "BID" | "OFFER" = Math.random() < 0.5 ? "BID" : "OFFER";
      for (let k = 0; k < c.layerThreshold; k++) {
        seq += 1;
        const oid = nextOrder++;
        const price = anchor * (1 + (Math.random() - 0.5) * (c.layerBandPct / 100));
        const rec: HistoryRec = { seq, t: t + k * 100, kind: "order.created", market, order_id: oid, side, price };
        out.push(rec);
        open.push({ order_id: oid, market, side, price, t: t + k * 100 });
      }
      continue;
    }
    // Normal generation.
    if (r < 0.55 || open.length === 0) {
      const oid = nextOrder++;
      const side: "BID" | "OFFER" = Math.random() < 0.5 ? "BID" : "OFFER";
      const price = 10 + Math.random() * 10_000;
      seq += 1;
      out.push({ seq, t, kind: "order.created", market, order_id: oid, side, price });
      open.push({ order_id: oid, market, side, price, t });
    } else if (r < 0.75) {
      const idx = Math.floor(Math.random() * open.length);
      const o = open[idx];
      open.splice(idx, 1);
      seq += 1;
      out.push({ seq, t, kind: "order.cancelled", market: o.market, order_id: o.order_id, side: o.side, price: o.price });
    } else if (r < 0.9) {
      const idx = Math.floor(Math.random() * open.length);
      const o = open[idx];
      open.splice(idx, 1);
      const pid = `prop_${nextProposal++}`;
      seq += 1;
      out.push({ seq, t, kind: "order.filled", market: o.market, order_id: o.order_id, side: o.side, price: o.price, proposal_id: pid });
      // Sometimes leave the fill "stuck": don't emit settled to trigger stuck_settlement heuristic.
      if (Math.random() < 0.7) pendingFills.push({ proposal_id: pid, market: o.market, t });
    } else if (pendingFills.length > 0) {
      const pf = pendingFills.shift()!;
      seq += 1;
      out.push({ seq, t, kind: Math.random() < 0.9 ? "settlement.settled" : "settlement.failed", market: pf.market, proposal_id: pf.proposal_id });
    }
  }
  return out;
}
