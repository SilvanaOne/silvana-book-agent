"use client";

import type { ReplayState } from "@/lib/replay-engine";

type Props = Readonly<{ agent: ReplayState | null }>;

export function InfoGrid({ agent }: Props) {
  const c = agent?.config;
  const total = agent?.total ?? 0;
  const rejectPct = total > 0 ? ((agent!.rejects / total) * 100).toFixed(1) + "%" : "—";
  const topHits = agent ? Object.entries(agent.hitsByRule).sort((a, b) => b[1] - a[1]).slice(0, 4) : [];

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">audit-replay</span></div>
        <div className="kv-row"><span className="k">stream</span><span className="v">offline JSONL</span></div>
        <div className="kv-row"><span className="k">emit accepts</span><span className="v accent">{c?.emitAccepts ? "yes" : "rejects only"}</span></div>
        <div className="kv-row"><span className="k">runs</span><span className="v">{agent?.runs ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">INPUT</div></div>
        <div className="kv-row"><span className="k">history size</span><span className="v">{c?.historySize ?? 0}</span></div>
        <div className="kv-row"><span className="k">party pool</span><span className="v">{c?.parties.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">market pool</span><span className="v">{c?.markets.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">evaluated</span><span className="v">{total}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">VERDICTS</div></div>
        <div className="kv-row"><span className="k">accepts</span><span className="v positive">{agent?.accepts ?? 0}</span></div>
        <div className="kv-row"><span className="k">rejects</span><span className="v negative">{agent?.rejects ?? 0}</span></div>
        <div className="kv-row"><span className="k">reject rate</span><span className="v">{rejectPct}</span></div>
        <div className="kv-row"><span className="k">buffer</span><span className="v">{agent?.verdicts.length ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RULE HITS</div></div>
        {topHits.length > 0 ? topHits.map(([rule, n]) => (
          <div key={rule} className="kv-row">
            <span className="k mono" style={{ fontSize: 11 }}>{truncate(rule, 22)}</span>
            <span className="v negative">{n}</span>
          </div>
        )) : (<>
          <div className="kv-row"><span className="k">rule 1</span><span className="v faint">—</span></div>
          <div className="kv-row"><span className="k">rule 2</span><span className="v faint">—</span></div>
          <div className="kv-row"><span className="k">rule 3</span><span className="v faint">—</span></div>
          <div className="kv-row"><span className="k">rule 4</span><span className="v faint">—</span></div>
        </>)}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RULES</div></div>
        <div className="kv-row"><span className="k">blocked mkts</span><span className="v">{c?.rules.blockedMarkets.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">allowed mkts</span><span className="v">{c?.rules.allowedMarkets.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">per-market caps</span><span className="v">{Object.keys(c?.rules.perMarketMaxDailyNotional ?? {}).length}</span></div>
        <div className="kv-row"><span className="k">max notional</span><span className="v">{c?.rules.maxNotionalPerOrder ?? "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATE</div></div>
        <div className="kv-row"><span className="k">status</span><span className="v">{agent ? "loaded" : "idle"}</span></div>
        <div className="kv-row"><span className="k">unique rules</span><span className="v">{Object.keys(agent?.hitsByRule ?? {}).length}</span></div>
        <div className="kv-row"><span className="k">notional cap</span><span className="v">{c?.rules.maxNotionalPerOrder ? "on" : "off"}</span></div>
        <div className="kv-row"><span className="k">price band</span><span className="v">{c?.rules.minPrice !== undefined || c?.rules.maxPrice !== undefined ? "on" : "off"}</span></div>
      </div>
    </div>
  );
}

function truncate(s: string, n: number): string { return s.length <= n ? s : s.slice(0, n - 1) + "…"; }
