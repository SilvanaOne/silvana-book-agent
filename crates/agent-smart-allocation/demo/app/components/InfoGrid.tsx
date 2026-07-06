"use client";

import type { SmartAllocState } from "@/lib/smartalloc-engine";

type Props = Readonly<{ agent: SmartAllocState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ agent, walk }: Props) {
  const c = agent?.config;
  const snap = agent?.lastSnapshot;
  const nextIn = agent?.nextCheckAt ? Math.max(0, Math.round((agent.nextCheckAt - Date.now()) / 1000)) : null;
  const stateLabel = agent === null ? "idle" : agent.status === "running" ? "monitoring" : "stopped";
  const breached = snap?.bucketRows.filter((r) => r.breached).length ?? 0;
  const totalBuckets = snap?.bucketRows.length ?? c?.buckets.length ?? 0;

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">smart-allocation</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">tick interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">check interval</span><span className="v">{c ? c.checkIntervalSecs + " s" : "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">POLICY</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">buckets</span><span className="v accent">{c.buckets.length}</span></div>
            <div className="kv-row"><span className="k">instruments</span><span className="v">{c.buckets.reduce((a, b) => a + b.instruments.length, 0)}</span></div>
            <div className="kv-row"><span className="k">threshold</span><span className="v">{c.bucketThresholdPct.toFixed(2)}%</span></div>
            <div className="kv-row"><span className="k">rebal fraction</span><span className="v">{(c.rebalanceFraction * 100).toFixed(0)}%</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">buckets</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">instruments</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">threshold</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">rebal fraction</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PORTFOLIO</div></div>
        <div className="kv-row"><span className="k">value (quote)</span><span className="v">{snap ? snap.portfolioValue.toFixed(2) : "—"}</span></div>
        <div className="kv-row"><span className="k">buckets breached</span><span className={`v ${breached > 0 ? "negative" : "positive"}`}>{breached} / {totalBuckets}</span></div>
        <div className="kv-row"><span className="k">last cycle</span><span className="v">{snap ? "#" + snap.seq : "—"}</span></div>
        <div className="kv-row"><span className="k">next check</span><span className="v">{nextIn === null ? "—" : nextIn + "s"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">REBALANCE</div></div>
        <div className="kv-row"><span className="k">cycles</span><span className="v">{agent?.cycle ?? 0}</span></div>
        <div className="kv-row"><span className="k">rebalances</span><span className="v accent">{agent?.totalRebalances ?? 0}</span></div>
        <div className="kv-row"><span className="k">total legs</span><span className="v">{agent?.totalLegs ?? 0}</span></div>
        <div className="kv-row"><span className="k">notional moved</span><span className="v">{(agent?.totalNotional ?? 0).toFixed(2)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">BUCKET WEIGHTS</div></div>
        {snap && snap.bucketRows.length > 0 ? (
          snap.bucketRows.slice(0, 4).map((r) => (
            <div key={r.name} className="kv-row">
              <span className="k mono" style={{ fontSize: 11 }}>{r.name}</span>
              <span className="v mono" style={{ fontSize: 11 }}>
                {(r.currentWeight * 100).toFixed(1)}%
                <span style={{ color: r.breached ? "var(--neg)" : "var(--text-faint)", marginLeft: 4 }}>
                  / {(r.targetWeight * 100).toFixed(1)}%
                </span>
              </span>
            </div>
          ))
        ) : (
          <>
            <div className="kv-row"><span className="k">bucket 1</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">bucket 2</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">bucket 3</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">bucket 4</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATE / SIM</div></div>
        <div className="kv-row"><span className="k">status</span><span className="v">{stateLabel}</span></div>
        <div className="kv-row"><span className="k">balance drift</span><span className="v">{c?.balanceDriftPerCycle ?? "—"}</span></div>
        <div className="kv-row"><span className="k">price vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
        <div className="kv-row"><span className="k">history len</span><span className="v">{agent?.weightHistory.length ?? 0}</span></div>
      </div>
    </div>
  );
}
