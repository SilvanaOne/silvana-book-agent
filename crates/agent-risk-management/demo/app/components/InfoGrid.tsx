"use client";

import type { RiskState } from "@/lib/riskmgmt-engine";

type Props = Readonly<{ agent: RiskState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ agent, walk }: Props) {
  const c = agent?.config;
  const p = c?.policy;
  const last = agent?.lastEvent;
  const nextIn = agent?.nextRunAt ? Math.max(0, Math.round((agent.nextRunAt - Date.now()) / 1000)) : null;
  const stateLabel = agent === null ? "idle" : agent.status === "running" ? "monitoring" : "stopped";
  const hasHits = (last?.hits.length ?? 0) > 0;

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">risk-management</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">tick interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">check interval</span><span className="v">{c ? c.checkIntervalSecs + " s" : "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">POLICY</div></div>
        {p ? (
          <>
            <div className="kv-row"><span className="k">max open orders</span><span className="v">{p.maxOpenOrders ?? "—"}</span></div>
            <div className="kv-row"><span className="k">max open notional</span><span className="v">{p.maxOpenNotional ?? "—"}</span></div>
            <div className="kv-row"><span className="k">max pending</span><span className="v">{p.maxPendingSettlements ?? "—"}</span></div>
            <div className="kv-row"><span className="k">max failed</span><span className="v">{p.maxFailedSettlements ?? "—"}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">max open orders</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">max open notional</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">max pending</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">max failed</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LIVE STATE</div></div>
        <div className="kv-row"><span className="k">open orders</span><span className="v">{last?.openOrders ?? agent?.orders.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">open notional</span><span className={`v ${hasHits ? "negative" : ""}`}>{last?.openNotional ?? "0.00"}</span></div>
        <div className="kv-row"><span className="k">pending settle</span><span className="v">{last?.pending ?? 0}</span></div>
        <div className="kv-row"><span className="k">failed settle</span><span className={`v ${(last?.failed ?? 0) > 0 ? "negative" : ""}`}>{last?.failed ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LAST CHECK</div></div>
        {last ? (
          <>
            <div className="kv-row"><span className="k">cycle</span><span className="v">#{last.seq}</span></div>
            <div className="kv-row"><span className="k">hits</span><span className={`v ${last.hits.length > 0 ? "negative" : "positive"}`}>{last.hits.length}</span></div>
            <div className="kv-row"><span className="k">enforced cancels</span><span className="v">{last.enforcedCancellations}</span></div>
            <div className="kv-row"><span className="k">next in</span><span className="v">{nextIn === null ? "—" : nextIn + "s"}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">cycle</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">hits</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">enforced cancels</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">next in</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">CUMULATIVE</div></div>
        <div className="kv-row"><span className="k">cycles run</span><span className="v">{agent?.cycle ?? 0}</span></div>
        <div className="kv-row"><span className="k">total breaches</span><span className={`v ${(agent?.totalBreaches ?? 0) > 0 ? "negative" : ""}`}>{agent?.totalBreaches ?? 0}</span></div>
        <div className="kv-row"><span className="k">total cancels</span><span className="v">{agent?.totalCancelled ?? 0}</span></div>
        <div className="kv-row"><span className="k">enforce mode</span><span className="v accent">{c?.enforce ? "ON" : "OFF"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">MARKET SIM</div></div>
        <div className="kv-row"><span className="k">markets</span><span className="v">{c?.markets.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">spawn / cycle</span><span className="v">{c?.spawnPerCycle ?? 0}</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>
    </div>
  );
}
