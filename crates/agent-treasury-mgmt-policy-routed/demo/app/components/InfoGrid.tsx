"use client";

import type { TreasuryState } from "@/lib/treasury-engine";

type Props = Readonly<{ agent: TreasuryState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ agent, walk }: Props) {
  const c = agent?.config;
  const snap = agent?.lastSnapshot;
  const nextIn = agent?.nextCheckAt ? Math.max(0, Math.round((agent.nextCheckAt - Date.now()) / 1000)) : null;
  const spent = agent?.dailyRolling.reduce((a, e) => a + e.notional, 0) ?? 0;
  const dailyPct = c ? Math.min(100, (spent / c.maxDailyTradeQuote) * 100) : 0;

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">treasury-mgmt</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">tick interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">check interval</span><span className="v">{c ? c.checkIntervalSecs + " s" : "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">POLICY</div></div>
        {c ? (<>
          <div className="kv-row"><span className="k">approval &gt;</span><span className="v accent">{c.approvalThresholdQuote}</span></div>
          <div className="kv-row"><span className="k">max / trade</span><span className="v">{c.maxTradeQuote}</span></div>
          <div className="kv-row"><span className="k">max / 24h</span><span className="v">{c.maxDailyTradeQuote}</span></div>
          <div className="kv-row"><span className="k">rebal frac</span><span className="v">{(c.rebalanceFraction * 100).toFixed(0)}%</span></div>
        </>) : (<>
          <div className="kv-row"><span className="k">approval &gt;</span><span className="v faint">—</span></div>
          <div className="kv-row"><span className="k">max / trade</span><span className="v faint">—</span></div>
          <div className="kv-row"><span className="k">max / 24h</span><span className="v faint">—</span></div>
          <div className="kv-row"><span className="k">rebal frac</span><span className="v faint">—</span></div>
        </>)}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PORTFOLIO</div></div>
        <div className="kv-row"><span className="k">value (quote)</span><span className="v">{snap ? snap.totalValue.toFixed(2) : "—"}</span></div>
        <div className="kv-row"><span className="k">breached</span><span className={`v ${(snap?.rows.filter((r) => r.breached).length ?? 0) > 0 ? "negative" : "positive"}`}>{snap?.rows.filter((r) => r.breached).length ?? 0} / {snap?.rows.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">cycle</span><span className="v">#{agent?.cycle ?? 0}</span></div>
        <div className="kv-row"><span className="k">next check</span><span className="v">{nextIn === null ? "—" : nextIn + "s"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">ROUTING</div></div>
        <div className="kv-row"><span className="k">direct</span><span className="v positive">{agent?.directCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">→ approval</span><span className="v accent">{agent?.approvalQueueCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">refused</span><span className="v negative">{agent?.refusedCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">total legs</span><span className="v">{agent?.legs.length ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">24H BUDGET</div></div>
        <div className="kv-row"><span className="k">spent</span><span className="v">{spent.toFixed(2)}</span></div>
        <div className="kv-row"><span className="k">cap</span><span className="v">{c?.maxDailyTradeQuote ?? "—"}</span></div>
        <div className="kv-row"><span className="k">utilisation</span><span className={`v ${dailyPct > 90 ? "negative" : dailyPct > 60 ? "accent" : "positive"}`}>{dailyPct.toFixed(1)}%</span></div>
        <div className="kv-row"><span className="k">entries</span><span className="v">{agent?.dailyRolling.length ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATE / SIM</div></div>
        <div className="kv-row"><span className="k">status</span><span className="v">{agent === null ? "idle" : agent.status === "running" ? "monitoring" : "stopped"}</span></div>
        <div className="kv-row"><span className="k">targets</span><span className="v">{c?.targets.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">balance drift</span><span className="v">{c?.balanceDriftPerCycle ?? "—"}</span></div>
        <div className="kv-row"><span className="k">price vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>
    </div>
  );
}
