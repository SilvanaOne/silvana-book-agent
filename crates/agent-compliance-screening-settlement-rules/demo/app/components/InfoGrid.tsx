"use client";

import type { ComplianceState } from "@/lib/compliance-engine";

type Props = Readonly<{ agent: ComplianceState | null }>;

export function InfoGrid({ agent }: Props) {
  const c = agent?.config;
  const p = c?.policy;
  const total = agent ? agent.accepts + agent.rejects : 0;
  const rejectPct = total > 0 ? ((agent!.rejects / total) * 100).toFixed(1) + "%" : "—";
  const topHits = agent ? Object.entries(agent.hitsByRule).sort((a, b) => b[1] - a[1]).slice(0, 3) : [];

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">compliance-screening</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">tick interval</span><span className="v">500 ms</span></div>
        <div className="kv-row"><span className="k">emit accepts</span><span className="v accent">{c?.emitAccepts ? "yes" : "rejects only"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">POLICY</div></div>
        <div className="kv-row"><span className="k">blocked pairs</span><span className="v">{p?.blockedPairs.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">party caps</span><span className="v">{p?.partyCaps.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">allow whitelist</span><span className="v">{p?.allowedCounterparties.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">blocked markets</span><span className="v">{p?.blockedMarkets.length ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TRAFFIC</div></div>
        <div className="kv-row"><span className="k">settlements</span><span className="v">{agent?.eventsSeen ?? 0}</span></div>
        <div className="kv-row"><span className="k">rate / sec</span><span className="v">{c?.eventRatePerSec ?? "—"}</span></div>
        <div className="kv-row"><span className="k">parties pool</span><span className="v">{c?.parties.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">markets pool</span><span className="v">{c?.markets.length ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">VERDICTS</div></div>
        <div className="kv-row"><span className="k">accepts</span><span className="v positive">{agent?.accepts ?? 0}</span></div>
        <div className="kv-row"><span className="k">rejects</span><span className="v negative">{agent?.rejects ?? 0}</span></div>
        <div className="kv-row"><span className="k">reject rate</span><span className="v">{rejectPct}</span></div>
        <div className="kv-row"><span className="k">filter</span><span className="v mono" style={{ fontSize: 11 }}>{c?.market ?? "all markets"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TOP HITS</div></div>
        {topHits.length > 0 ? topHits.map(([rule, n]) => (
          <div key={rule} className="kv-row">
            <span className="k mono" style={{ fontSize: 11 }}>{truncate(rule, 20)}</span>
            <span className="v negative">{n}</span>
          </div>
        )) : (
          <>
            <div className="kv-row"><span className="k">rule 1</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">rule 2</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">rule 3</span><span className="v faint">—</span></div>
          </>
        )}
        <div className="kv-row"><span className="k">total unique</span><span className="v">{Object.keys(agent?.hitsByRule ?? {}).length}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATE</div></div>
        <div className="kv-row"><span className="k">status</span><span className="v">{agent === null ? "idle" : agent.status === "running" ? "screening" : "stopped"}</span></div>
        <div className="kv-row"><span className="k">stream</span><span className="v">SubscribeSettlements</span></div>
        <div className="kv-row"><span className="k">parties tracked</span><span className="v">{Object.keys(agent?.partyTotals ?? {}).length}</span></div>
        <div className="kv-row"><span className="k">buffer</span><span className="v">{agent?.recentEvals.length ?? 0} evals</span></div>
      </div>
    </div>
  );
}

function truncate(s: string, n: number): string { return s.length <= n ? s : s.slice(0, n - 1) + "…"; }
