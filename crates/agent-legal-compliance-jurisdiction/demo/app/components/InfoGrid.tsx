"use client";

import type { LegalState } from "@/lib/legal-engine";

type Props = Readonly<{ agent: LegalState | null }>;

export function InfoGrid({ agent }: Props) {
  const c = agent?.config;
  const total = agent ? agent.legal + agent.violation : 0;
  const violPct = total > 0 ? ((agent!.violation / total) * 100).toFixed(1) + "%" : "—";
  const topHits = agent ? Object.entries(agent.hitsByRule).sort((a, b) => b[1] - a[1]).slice(0, 3) : [];
  const jxCount = Object.keys(c?.policy.jurisdictions ?? {}).length;

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">legal-compliance</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">tick interval</span><span className="v">500 ms</span></div>
        <div className="kv-row"><span className="k">jurisdictions</span><span className="v accent">{jxCount}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">POLICY</div></div>
        <div className="kv-row"><span className="k">parties mapped</span><span className="v">{Object.keys(c?.policy.partyJurisdictions ?? {}).length}</span></div>
        <div className="kv-row"><span className="k">party pool</span><span className="v">{c?.parties.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">market pool</span><span className="v">{c?.markets.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">rate / sec</span><span className="v">{c?.eventRatePerSec ?? "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">VERDICTS</div></div>
        <div className="kv-row"><span className="k">settlements</span><span className="v">{agent?.settlementsSeen ?? 0}</span></div>
        <div className="kv-row"><span className="k">legal</span><span className="v positive">{agent?.legal ?? 0}</span></div>
        <div className="kv-row"><span className="k">violations</span><span className="v negative">{agent?.violation ?? 0}</span></div>
        <div className="kv-row"><span className="k">violation rate</span><span className="v">{violPct}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TOP RULES</div></div>
        {topHits.length > 0 ? topHits.map(([rule, n]) => (
          <div key={rule} className="kv-row">
            <span className="k mono" style={{ fontSize: 11 }}>{truncate(rule, 22)}</span>
            <span className="v negative">{n}</span>
          </div>
        )) : (
          <>
            <div className="kv-row"><span className="k">rule 1</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">rule 2</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">rule 3</span><span className="v faint">—</span></div>
          </>
        )}
        <div className="kv-row"><span className="k">unique rules</span><span className="v">{Object.keys(agent?.hitsByRule ?? {}).length}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">JX ACTIVITY</div></div>
        {agent && Object.keys(agent.perJurisdiction).length > 0 ? Object.entries(agent.perJurisdiction).slice(0, 4).map(([jx, b]) => (
          <div key={jx} className="kv-row">
            <span className="k">{jx}</span>
            <span className="v mono" style={{ fontSize: 11 }}>{b.total} · <span className={b.violations > 0 ? "negative" : "positive"}>{b.violations} viol</span></span>
          </div>
        )) : (
          <>
            <div className="kv-row"><span className="k">jx 1</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">jx 2</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">jx 3</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">jx 4</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATE</div></div>
        <div className="kv-row"><span className="k">status</span><span className="v">{agent === null ? "idle" : agent.status === "running" ? "evaluating" : "stopped"}</span></div>
        <div className="kv-row"><span className="k">enforce</span><span className="v">read-only</span></div>
        <div className="kv-row"><span className="k">stream</span><span className="v">SubscribeSettlements</span></div>
        <div className="kv-row"><span className="k">reload</span><span className="v">on restart</span></div>
      </div>
    </div>
  );
}

function truncate(s: string, n: number): string { return s.length <= n ? s : s.slice(0, n - 1) + "…"; }
