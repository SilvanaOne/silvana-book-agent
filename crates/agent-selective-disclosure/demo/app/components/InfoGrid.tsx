"use client";

import type { DiscloseState } from "@/lib/disclose-engine";

type Props = Readonly<{ agent: DiscloseState | null }>;

export function InfoGrid({ agent }: Props) {
  const c = agent?.config;
  const verify = agent?.lastVerify;

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">selective-disclosure</span></div>
        <div className="kv-row"><span className="k">signing</span><span className="v">ed25519-sha256-v1</span></div>
        <div className="kv-row"><span className="k">chain kind</span><span className="v">prev_hash sha256</span></div>
        <div className="kv-row"><span className="k">demo hash</span><span className="v faint">faux (not crypto)</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">FILTERS</div></div>
        <div className="kv-row"><span className="k">keep kinds</span><span className="v accent">{c?.kinds.length ? c.kinds.length : "all"}</span></div>
        <div className="kv-row"><span className="k">keep markets</span><span className="v accent">{c?.markets.length ? c.markets.length : "all"}</span></div>
        <div className="kv-row"><span className="k">redact fields</span><span className="v">{c?.redactFields.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">runs</span><span className="v">{agent?.runs ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RECORDS</div></div>
        <div className="kv-row"><span className="k">history in</span><span className="v">{agent?.history.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">disclosure out</span><span className="v accent">{agent?.disclosure.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">reduction</span><span className="v">{reductionPct(agent)}</span></div>
        <div className="kv-row"><span className="k">redacted fields</span><span className="v">{agent?.redactedFields ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">VERIFY</div></div>
        {verify ? (
          <>
            <div className="kv-row"><span className="k">verdict</span><span className={`v ${verify.ok ? "positive" : "negative"}`}>{verify.ok ? "OK" : "FAIL"}</span></div>
            <div className="kv-row"><span className="k">checked</span><span className="v">{verify.checked}</span></div>
            <div className="kv-row"><span className="k">broken at</span><span className="v">{verify.brokenAt ?? "—"}</span></div>
            <div className="kv-row"><span className="k">reason</span><span className="v mono" style={{ fontSize: 11 }}>{verify.reason ?? "—"}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">verdict</span><span className="v faint">not run</span></div>
            <div className="kv-row"><span className="k">checked</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">broken at</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">reason</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">KIND BREAKDOWN</div></div>
        {kindBreakdown(agent).map(([k, n]) => (
          <div key={k} className="kv-row">
            <span className="k mono" style={{ fontSize: 11 }}>{k}</span>
            <span className="v">{n}</span>
          </div>
        ))}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATE</div></div>
        <div className="kv-row"><span className="k">status</span><span className="v">{agent ? "loaded" : "idle"}</span></div>
        <div className="kv-row"><span className="k">party pool</span><span className="v">{c?.parties.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">market pool</span><span className="v">{c?.marketPool.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">chain scheme</span><span className="v">prev_hash</span></div>
      </div>
    </div>
  );
}

function reductionPct(agent: DiscloseState | null): string {
  if (!agent || agent.history.length === 0) return "—";
  const kept = (agent.disclosure.length / agent.history.length) * 100;
  return kept.toFixed(1) + "% kept";
}

function kindBreakdown(agent: DiscloseState | null): Array<[string, number]> {
  if (!agent) return [["kind 1", 0], ["kind 2", 0], ["kind 3", 0], ["kind 4", 0]];
  const map: Record<string, number> = {};
  for (const rec of agent.disclosure) map[rec.kind] = (map[rec.kind] ?? 0) + 1;
  return Object.entries(map).sort((a, b) => b[1] - a[1]).slice(0, 4);
}
