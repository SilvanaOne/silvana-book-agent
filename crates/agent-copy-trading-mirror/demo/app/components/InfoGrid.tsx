"use client";

import type { CopyState } from "@/lib/copy-engine";

type Props = Readonly<{ agent: CopyState | null }>;

export function InfoGrid({ agent }: Props) {
  const c = agent?.config;
  const total = agent ? agent.totalMirrored + agent.totalRejected : 0;
  const mirrorPct = total > 0 ? ((agent!.totalMirrored / total) * 100).toFixed(1) + "%" : "—";
  const topRefusals = agent ? Object.entries(agent.refusedByRule).sort((a, b) => b[1] - a[1]).slice(0, 3) : [];

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">copy-trading</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">tick interval</span><span className="v">500 ms</span></div>
        <div className="kv-row"><span className="k">dry run</span><span className="v accent">{c?.dryRun ? "yes" : "live"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PARTIES</div></div>
        <div className="kv-row"><span className="k">leader</span><span className="v mono" style={{ fontSize: 11 }}>{c?.leader ?? "—"}</span></div>
        <div className="kv-row"><span className="k">follower</span><span className="v mono" style={{ fontSize: 11 }}>{c?.follower ?? "—"}</span></div>
        <div className="kv-row"><span className="k">scale</span><span className="v accent">{c ? (c.scale * 100).toFixed(0) + "%" : "—"}</span></div>
        <div className="kv-row"><span className="k">leader rate</span><span className="v">{c?.leaderRatePerSec ?? "—"} /s</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TRAFFIC</div></div>
        <div className="kv-row"><span className="k">leader orders</span><span className="v">{agent?.totalLeader ?? 0}</span></div>
        <div className="kv-row"><span className="k">mirrored</span><span className="v positive">{agent?.totalMirrored ?? 0}</span></div>
        <div className="kv-row"><span className="k">rejected</span><span className={`v ${(agent?.totalRejected ?? 0) > 0 ? "negative" : ""}`}>{agent?.totalRejected ?? 0}</span></div>
        <div className="kv-row"><span className="k">mirror rate</span><span className="v">{mirrorPct}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">FILTERS</div></div>
        <div className="kv-row"><span className="k">markets whitelist</span><span className="v">{c?.markets.length ? c.markets.length : "all"}</span></div>
        <div className="kv-row"><span className="k">max leader ntl</span><span className="v">{c?.maxLeaderNotional ?? "—"}</span></div>
        <div className="kv-row"><span className="k">max mirror ntl</span><span className="v">{c?.maxMirrorNotional ?? "—"}</span></div>
        <div className="kv-row"><span className="k">pool</span><span className="v">{c?.marketPool.length ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TOP REFUSALS</div></div>
        {topRefusals.length > 0 ? topRefusals.map(([rule, n]) => (
          <div key={rule} className="kv-row">
            <span className="k mono" style={{ fontSize: 11 }}>{rule}</span>
            <span className="v negative">{n}</span>
          </div>
        )) : (<>
          <div className="kv-row"><span className="k">rule 1</span><span className="v faint">—</span></div>
          <div className="kv-row"><span className="k">rule 2</span><span className="v faint">—</span></div>
          <div className="kv-row"><span className="k">rule 3</span><span className="v faint">—</span></div>
        </>)}
        <div className="kv-row"><span className="k">unique</span><span className="v">{Object.keys(agent?.refusedByRule ?? {}).length}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATE</div></div>
        <div className="kv-row"><span className="k">status</span><span className="v">{agent === null ? "idle" : agent.status === "running" ? "mirroring" : "stopped"}</span></div>
        <div className="kv-row"><span className="k">markets touched</span><span className="v">{Object.keys(agent?.leaderPosByMarket ?? {}).length}</span></div>
        <div className="kv-row"><span className="k">leader buffer</span><span className="v">{agent?.leaderOrders.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">mirrors buffer</span><span className="v">{agent?.mirrors.length ?? 0}</span></div>
      </div>
    </div>
  );
}
