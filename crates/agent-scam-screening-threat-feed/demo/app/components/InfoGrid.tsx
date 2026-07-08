"use client";

import type { ScamState } from "@/lib/scam-engine";

type Props = Readonly<{ agent: ScamState | null }>;

export function InfoGrid({ agent }: Props) {
  const c = agent?.config;
  const totalMatched = agent ? agent.info + agent.warn + agent.critical : 0;
  const total = agent ? totalMatched + agent.clean : 0;
  const matchPct = total > 0 ? ((totalMatched / total) * 100).toFixed(1) + "%" : "—";
  const topCats = agent ? Object.entries(agent.perCategoryHits).sort((a, b) => b[1] - a[1]).slice(0, 3) : [];

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">scam-screening</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">tick interval</span><span className="v">500 ms</span></div>
        <div className="kv-row"><span className="k">categories</span><span className="v accent">{c?.categories.length ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">FEED</div></div>
        <div className="kv-row"><span className="k">refresh every</span><span className="v">{c ? c.refreshSecs + "s" : "—"}</span></div>
        <div className="kv-row"><span className="k">refreshes</span><span className="v">{agent?.refreshes ?? 0}</span></div>
        <div className="kv-row"><span className="k">tracked parties</span><span className="v">{c?.categories.reduce((a, x) => a + x.parties.length, 0) ?? 0}</span></div>
        <div className="kv-row"><span className="k">rate / sec</span><span className="v">{c?.eventRatePerSec ?? "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TRAFFIC</div></div>
        <div className="kv-row"><span className="k">settlements</span><span className="v">{agent?.settlementsSeen ?? 0}</span></div>
        <div className="kv-row"><span className="k">clean</span><span className="v positive">{agent?.clean ?? 0}</span></div>
        <div className="kv-row"><span className="k">matched</span><span className={`v ${totalMatched > 0 ? "negative" : ""}`}>{totalMatched}</span></div>
        <div className="kv-row"><span className="k">match rate</span><span className="v">{matchPct}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">SEVERITY</div></div>
        <div className="kv-row"><span className="k">info</span><span className="v">{agent?.info ?? 0}</span></div>
        <div className="kv-row"><span className="k">warn</span><span className="v accent">{agent?.warn ?? 0}</span></div>
        <div className="kv-row"><span className="k">critical</span><span className="v negative">{agent?.critical ?? 0}</span></div>
        <div className="kv-row"><span className="k">worst hit</span><span className="v">{worstSev(agent)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TOP CATS</div></div>
        {topCats.length > 0 ? topCats.map(([cat, n]) => (
          <div key={cat} className="kv-row">
            <span className="k mono" style={{ fontSize: 11 }}>{cat}</span>
            <span className="v negative">{n}</span>
          </div>
        )) : (
          <>
            <div className="kv-row"><span className="k">cat 1</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">cat 2</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">cat 3</span><span className="v faint">—</span></div>
          </>
        )}
        <div className="kv-row"><span className="k">unique hits</span><span className="v">{Object.keys(agent?.perCategoryHits ?? {}).length}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATE</div></div>
        <div className="kv-row"><span className="k">status</span><span className="v">{agent === null ? "idle" : agent.status === "running" ? "screening" : "stopped"}</span></div>
        <div className="kv-row"><span className="k">market pool</span><span className="v">{c?.markets.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">party pool</span><span className="v">{c?.parties.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">buffer</span><span className="v">{agent?.recentEvals.length ?? 0}</span></div>
      </div>
    </div>
  );
}

function worstSev(agent: ScamState | null): string {
  if (!agent) return "—";
  if (agent.critical > 0) return "critical";
  if (agent.warn > 0) return "warn";
  if (agent.info > 0) return "info";
  return "—";
}
