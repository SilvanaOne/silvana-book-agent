"use client";

import type { TargetAllocationState } from "@/lib/targetallocation-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

function pct(n: number | null | undefined): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const s = n >= 0 ? "+" : "";
  return `${s}${n.toFixed(2)}%`;
}

function sgn(n: number): string { return n >= 0 ? "+" : "−"; }

type Props = Readonly<{ targetallocation: TargetAllocationState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ targetallocation, walk }: Props) {
  const c = targetallocation?.config;
  const positions = targetallocation?.positions ?? [];
  const totalCurrent = positions.reduce((s, p) => s + p.currentQuote, 0);
  const totalTarget = positions.reduce((s, p) => s + p.targetQuote, 0);
  const totalDev = positions.reduce((s, p) => s + Math.abs(p.deviationQuote), 0);
  const openOrders = targetallocation?.orders.filter((o) => o.status === "open").length ?? 0;
  const stateLabel = targetallocation === null ? "idle" : targetallocation.status === "monitoring" ? "monitoring" : "stopped";

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">target-allocation</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TA CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">targets</span><span className="v">{c.targets.length}</span></div>
            <div className="kv-row"><span className="k">threshold (quote)</span><span className="v accent">{fmt(c.thresholdQuote)}</span></div>
            <div className="kv-row"><span className="k">rebalance frac</span><span className="v">{c.rebalanceFraction}</span></div>
            <div className="kv-row"><span className="k">check interval</span><span className="v">{c.checkIntervalSecs}s</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">targets</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">threshold (quote)</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">rebalance frac</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">check interval</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PORTFOLIO</div></div>
        <div className="kv-row"><span className="k">total current</span><span className="v">{fmt(totalCurrent)}</span></div>
        <div className="kv-row"><span className="k">total target</span><span className="v accent">{fmt(totalTarget)}</span></div>
        <div className="kv-row"><span className="k">absolute dev</span><span className={`v ${totalDev > (c?.thresholdQuote ?? Infinity) ? "negative" : "positive"}`}>{fmt(totalDev)}</span></div>
        <div className="kv-row"><span className="k">base price</span><span className="v">{fmt(targetallocation?.currentPrice)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">ALLOCATIONS</div></div>
        {positions.length === 0 && <div className="kv-row"><span className="k">—</span><span className="v faint">no positions</span></div>}
        {positions.map((p) => {
          const pctDev = p.targetQuote > 0 ? (p.deviationQuote / p.targetQuote) * 100 : 0;
          const cls = Math.abs(p.deviationQuote) > (c?.thresholdQuote ?? Infinity)
            ? (p.deviationQuote > 0 ? "negative" : "accent") : "";
          return (
            <div className="kv-row" key={p.instrument + p.market}>
              <span className="k">{p.instrument}</span>
              <span className={`v ${cls}`}>{fmt(p.currentQuote)} / {fmt(p.targetQuote)} ({sgn(p.deviationQuote)}{fmt(Math.abs(p.deviationQuote))}, {pct(pctDev)})</span>
            </div>
          );
        })}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">ORDERS</div></div>
        <div className="kv-row"><span className="k">orders placed</span><span className="v">{targetallocation?.ordersPlaced ?? 0}</span></div>
        <div className="kv-row"><span className="k">orders filled</span><span className="v accent">{targetallocation?.ordersFilled ?? 0}</span></div>
        <div className="kv-row"><span className="k">open orders</span><span className="v">{openOrders}</span></div>
        <div className="kv-row"><span className="k">last check</span><span className="v">{targetallocation?.lastCheckAt ? new Date(targetallocation.lastCheckAt).toLocaleTimeString() : "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATE</div></div>
        <div className="kv-row"><span className="k">status</span><span className="v">{stateLabel}</span></div>
        <div className="kv-row"><span className="k">price source</span><span className="v">GBM walk</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>
    </div>
  );
}
