"use client";

import type { ClassifierState } from "@/lib/classifier-engine";

type Props = Readonly<{ agent: ClassifierState | null }>;

export function InfoGrid({ agent }: Props) {
  const c = agent?.config;
  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">anomaly-classifier</span></div>
        <div className="kv-row"><span className="k">classifier</span><span className="v mono" style={{ fontSize: 11 }}>rules-v1</span></div>
        <div className="kv-row"><span className="k">tick interval</span><span className="v">500 ms</span></div>
        <div className="kv-row"><span className="k">rate</span><span className="v accent">{c?.ratePerSec ?? "—"} /s</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RULES</div></div>
        <div className="kv-row"><span className="k">window</span><span className="v">{c?.windowSecs ?? "—"}s</span></div>
        <div className="kv-row"><span className="k">cluster thr</span><span className="v">{c?.clusterThreshold ?? "—"}</span></div>
        <div className="kv-row"><span className="k">inject spoofing</span><span className="v accent">{c?.injectSpoofing ? "yes" : "no"}</span></div>
        <div className="kv-row"><span className="k">inject wash</span><span className="v accent">{c?.injectWashTrading ? "yes" : "no"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">CLASSES</div></div>
        <div className="kv-row"><span className="k">spoofing</span><span className="v negative">{agent?.byClass.spoofing ?? 0}</span></div>
        <div className="kv-row"><span className="k">wash_trading</span><span className="v" style={{ color: "#ffd66b" }}>{agent?.byClass.wash_trading ?? 0}</span></div>
        <div className="kv-row"><span className="k">stuck</span><span className="v accent">{agent?.byClass.stuck_settlement ?? 0}</span></div>
        <div className="kv-row"><span className="k">normal_vol</span><span className="v positive">{agent?.byClass.normal_volatility ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">SOURCE KINDS</div></div>
        <div className="kv-row"><span className="k">layer_cluster</span><span className="v">{agent?.bySourceKind["anomaly.layer_cluster"] ?? 0}</span></div>
        <div className="kv-row"><span className="k">rapid_cancel</span><span className="v">{agent?.bySourceKind["anomaly.rapid_cancel"] ?? 0}</span></div>
        <div className="kv-row"><span className="k">fill+burst</span><span className="v">{agent?.bySourceKind["anomaly.fill_before_cancel_burst"] ?? 0}</span></div>
        <div className="kv-row"><span className="k">stuck_settle</span><span className="v">{agent?.bySourceKind["anomaly.stuck_settlement"] ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TRAFFIC</div></div>
        <div className="kv-row"><span className="k">seen</span><span className="v">{agent?.totalSeen ?? 0}</span></div>
        <div className="kv-row"><span className="k">markets</span><span className="v">{Object.keys(agent?.byMarket ?? {}).length}</span></div>
        <div className="kv-row"><span className="k">buffer</span><span className="v">{agent?.anomalies.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">worst market</span><span className="v mono" style={{ fontSize: 11 }}>{worstMarket(agent)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATE</div></div>
        <div className="kv-row"><span className="k">status</span><span className="v">{agent === null ? "idle" : agent.status === "running" ? "classifying" : "stopped"}</span></div>
        <div className="kv-row"><span className="k">windows</span><span className="v">{Object.keys(agent?.windows ?? {}).length}</span></div>
        <div className="kv-row"><span className="k">worst class</span><span className="v mono" style={{ fontSize: 11 }}>{worstClass(agent)}</span></div>
        <div className="kv-row"><span className="k">consumes</span><span className="v">audit-anomaly</span></div>
      </div>
    </div>
  );
}

function worstMarket(agent: ClassifierState | null): string {
  if (!agent) return "—";
  const bad = agent.anomalies.filter((a) => a.classLabel === "spoofing" || a.classLabel === "wash_trading");
  const counts: Record<string, number> = {};
  for (const a of bad) counts[a.market] = (counts[a.market] ?? 0) + 1;
  const sorted = Object.entries(counts).sort((a, b) => b[1] - a[1]);
  return sorted.length > 0 ? `${sorted[0][0]} (${sorted[0][1]})` : "—";
}

function worstClass(agent: ClassifierState | null): string {
  if (!agent) return "—";
  if ((agent.byClass.spoofing ?? 0) > 0) return "spoofing";
  if ((agent.byClass.wash_trading ?? 0) > 0) return "wash_trading";
  if ((agent.byClass.stuck_settlement ?? 0) > 0) return "stuck_settlement";
  return "normal";
}
