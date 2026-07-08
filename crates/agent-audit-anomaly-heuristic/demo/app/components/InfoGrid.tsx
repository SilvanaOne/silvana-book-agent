"use client";

import type { AnomalyState } from "@/lib/anomaly-engine";

type Props = Readonly<{ agent: AnomalyState | null }>;

export function InfoGrid({ agent }: Props) {
  const c = agent?.config;
  const total = agent?.anomalies.length ?? 0;

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">audit-anomaly</span></div>
        <div className="kv-row"><span className="k">stream</span><span className="v">offline JSONL</span></div>
        <div className="kv-row"><span className="k">scans</span><span className="v">{agent?.scans ?? 0}</span></div>
        <div className="kv-row"><span className="k">inject</span><span className="v accent">{c?.injectAnomalies ? "yes" : "natural only"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">INPUT</div></div>
        <div className="kv-row"><span className="k">days</span><span className="v">{c?.historyDays ?? 0}</span></div>
        <div className="kv-row"><span className="k">records / day</span><span className="v">{c?.recordsPerDay ?? 0}</span></div>
        <div className="kv-row"><span className="k">total records</span><span className="v">{agent?.history.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">markets</span><span className="v">{c?.markets.length ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">FINDINGS</div></div>
        <div className="kv-row"><span className="k">total</span><span className={`v ${total > 0 ? "negative" : "positive"}`}>{total}</span></div>
        <div className="kv-row"><span className="k">stuck settle</span><span className="v">{agent?.byKind.stuck_settlement ?? 0}</span></div>
        <div className="kv-row"><span className="k">rapid cancel</span><span className="v">{agent?.byKind.rapid_cancel ?? 0}</span></div>
        <div className="kv-row"><span className="k">layer cluster</span><span className="v">{agent?.byKind.layer_cluster ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">MORE FINDINGS</div></div>
        <div className="kv-row"><span className="k">fill+burst</span><span className="v">{agent?.byKind.fill_before_cancel_burst ?? 0}</span></div>
        <div className="kv-row"><span className="k">unique kinds</span><span className="v">{Object.keys(agent?.byKind ?? {}).length}</span></div>
        <div className="kv-row"><span className="k">hit rate</span><span className="v">{agent && agent.history.length > 0 ? ((total / agent.history.length) * 100).toFixed(2) + "%" : "—"}</span></div>
        <div className="kv-row"><span className="k">worst market</span><span className="v mono" style={{ fontSize: 11 }}>{worstMarket(agent)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">THRESHOLDS</div></div>
        <div className="kv-row"><span className="k">stuck window</span><span className="v">{c?.settlementWindowSecs ?? "—"}s</span></div>
        <div className="kv-row"><span className="k">rapid cancel</span><span className="v">{c?.rapidCancelWindowMs ?? "—"}ms</span></div>
        <div className="kv-row"><span className="k">layer thresh</span><span className="v">{c?.layerThreshold ?? "—"}</span></div>
        <div className="kv-row"><span className="k">burst</span><span className="v">{c?.burstCount ?? "—"} in {c?.burstWindowSecs ?? "—"}s</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATE</div></div>
        <div className="kv-row"><span className="k">status</span><span className="v">{agent ? "loaded" : "idle"}</span></div>
        <div className="kv-row"><span className="k">market pool</span><span className="v">{c?.markets.slice(0, 3).join(",") ?? "—"}</span></div>
        <div className="kv-row"><span className="k">band %</span><span className="v">{c?.layerBandPct.toFixed(2) ?? "—"}</span></div>
        <div className="kv-row"><span className="k">buffered</span><span className="v">{agent?.anomalies.length ?? 0}</span></div>
      </div>
    </div>
  );
}

function worstMarket(agent: AnomalyState | null): string {
  if (!agent) return "—";
  const counts: Record<string, number> = {};
  for (const a of agent.anomalies) counts[a.market] = (counts[a.market] ?? 0) + 1;
  const sorted = Object.entries(counts).sort((a, b) => b[1] - a[1]);
  return sorted.length > 0 ? `${sorted[0][0]} (${sorted[0][1]})` : "—";
}
