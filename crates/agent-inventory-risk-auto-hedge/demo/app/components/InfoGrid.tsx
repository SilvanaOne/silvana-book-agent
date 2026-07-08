"use client";

import type { InvRiskState, Zone } from "@/lib/invrisk-engine";

type Props = Readonly<{ agent: InvRiskState | null; walk: { driftPerTick: number; volPerTick: number } }>;

function zoneLabel(z: Zone): string {
  if (z === "ok") return "OK";
  if (z === "soft_band") return "SOFT BAND";
  return "HARD BAND";
}

function zoneClass(z: Zone): string {
  if (z === "ok") return "positive";
  if (z === "soft_band") return "accent";
  return "negative";
}

export function InfoGrid({ agent, walk }: Props) {
  const c = agent?.config;
  const stateLabel = agent === null ? "idle" : agent.status === "running" ? "monitoring" : "stopped";
  const delta = agent ? agent.balance - (c?.target ?? 0) : 0;
  const nextIn = agent?.nextCheckAt ? Math.max(0, Math.round((agent.nextCheckAt - Date.now()) / 1000)) : null;

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">inventory-risk</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">tick interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">check interval</span><span className="v">{c ? c.checkIntervalSecs + " s" : "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">BAND CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">instrument</span><span className="v accent">{c.instrument}</span></div>
            <div className="kv-row"><span className="k">hedge market</span><span className="v">{c.hedgeMarket}</span></div>
            <div className="kv-row"><span className="k">target ± soft</span><span className="v">{c.target} ± {c.softTolerance}</span></div>
            <div className="kv-row"><span className="k">± hard</span><span className="v">± {c.hardTolerance}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">instrument</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">hedge market</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">target ± soft</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">± hard</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">POSITION</div></div>
        {agent ? (
          <>
            <div className="kv-row"><span className="k">balance</span><span className="v">{agent.balance.toFixed(3)}</span></div>
            <div className="kv-row"><span className="k">delta</span><span className={`v ${delta > 0 ? "negative" : delta < 0 ? "accent" : "positive"}`}>{delta >= 0 ? "+" : ""}{delta.toFixed(3)}</span></div>
            <div className="kv-row"><span className="k">zone</span><span className={`v ${zoneClass(agent.currentZone)}`}>{zoneLabel(agent.currentZone)}</span></div>
            <div className="kv-row"><span className="k">mid</span><span className="v">{agent.currentPrice.toFixed(6)}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">balance</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">delta</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">zone</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">mid</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">SIGNALS</div></div>
        <div className="kv-row"><span className="k">cycles</span><span className="v">{agent?.cycle ?? 0}</span></div>
        <div className="kv-row"><span className="k">soft signals</span><span className="v accent">{agent?.softSignalsEmitted ?? 0}</span></div>
        <div className="kv-row"><span className="k">hard breaches</span><span className={`v ${(agent?.hardBreaches ?? 0) > 0 ? "negative" : ""}`}>{agent?.hardBreaches ?? 0}</span></div>
        <div className="kv-row"><span className="k">next check in</span><span className="v">{nextIn === null ? "—" : nextIn + "s"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">HEDGES</div></div>
        <div className="kv-row"><span className="k">placed</span><span className="v">{agent?.hedgesPlaced ?? 0}</span></div>
        <div className="kv-row"><span className="k">filled</span><span className="v positive">{agent?.hedgesFilled ?? 0}</span></div>
        <div className="kv-row"><span className="k">in-flight</span><span className="v">{agent?.activeHedges.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">total notional</span><span className="v">{(agent?.totalHedgeNotional ?? 0).toFixed(2)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATE / SIM</div></div>
        <div className="kv-row"><span className="k">status</span><span className="v">{stateLabel}</span></div>
        <div className="kv-row"><span className="k">auto_hedge</span><span className="v accent">{c?.autoHedge ? "ON" : "OFF"}</span></div>
        <div className="kv-row"><span className="k">exposure drift</span><span className="v">{c?.driftPerCycle ?? "—"}</span></div>
        <div className="kv-row"><span className="k">price vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>
    </div>
  );
}
