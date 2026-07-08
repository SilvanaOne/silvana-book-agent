"use client";

import type { NewsState } from "@/lib/news-engine";

type Props = Readonly<{ agent: NewsState | null }>;

export function InfoGrid({ agent }: Props) {
  const c = agent?.config;
  const total = agent?.totalSeen ?? 0;
  const emitPct = total > 0 ? ((agent!.totalEmitted / total) * 100).toFixed(1) + "%" : "—";
  const topMarkets = agent ? Object.entries(agent.byMarket).sort((a, b) => b[1] - a[1]).slice(0, 3) : [];

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">news-sentiment</span></div>
        <div className="kv-row"><span className="k">scorer</span><span className="v mono" style={{ fontSize: 11 }}>lexicon-v1</span></div>
        <div className="kv-row"><span className="k">tick interval</span><span className="v">500 ms</span></div>
        <div className="kv-row"><span className="k">rate</span><span className="v accent">{c?.headlinesPerSec ?? "—"} /s</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">CONFIG</div></div>
        <div className="kv-row"><span className="k">threshold</span><span className="v">{c ? c.threshold.toFixed(2) : "—"}</span></div>
        <div className="kv-row"><span className="k">markets</span><span className="v">{Object.keys(c?.aliases ?? {}).length}</span></div>
        <div className="kv-row"><span className="k">quantity</span><span className="v">{c?.quantity ?? "—"}</span></div>
        <div className="kv-row"><span className="k">pool size</span><span className="v">{c?.headlinePool.length ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TRAFFIC</div></div>
        <div className="kv-row"><span className="k">headlines seen</span><span className="v">{total}</span></div>
        <div className="kv-row"><span className="k">signals emitted</span><span className="v positive">{agent?.totalEmitted ?? 0}</span></div>
        <div className="kv-row"><span className="k">emit rate</span><span className="v">{emitPct}</span></div>
        <div className="kv-row"><span className="k">avg |score|</span><span className="v">{agent?.totalEmitted ? agent.avgScore.toFixed(2) : "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">SKIPPED</div></div>
        <div className="kv-row"><span className="k">no market</span><span className="v">{agent?.skippedNoMarket ?? 0}</span></div>
        <div className="kv-row"><span className="k">below threshold</span><span className="v">{agent?.skippedBelowThr ?? 0}</span></div>
        <div className="kv-row"><span className="k">buy signals</span><span className="v positive">{agent?.bySide.buy ?? 0}</span></div>
        <div className="kv-row"><span className="k">sell signals</span><span className="v negative">{agent?.bySide.sell ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TOP MARKETS</div></div>
        {topMarkets.length > 0 ? topMarkets.map(([m, n]) => (
          <div key={m} className="kv-row"><span className="k mono" style={{ fontSize: 11 }}>{m}</span><span className="v accent">{n}</span></div>
        )) : (<>
          <div className="kv-row"><span className="k">market 1</span><span className="v faint">—</span></div>
          <div className="kv-row"><span className="k">market 2</span><span className="v faint">—</span></div>
          <div className="kv-row"><span className="k">market 3</span><span className="v faint">—</span></div>
        </>)}
        <div className="kv-row"><span className="k">unique</span><span className="v">{Object.keys(agent?.byMarket ?? {}).length}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATE</div></div>
        <div className="kv-row"><span className="k">status</span><span className="v">{agent === null ? "idle" : agent.status === "running" ? "scoring" : "stopped"}</span></div>
        <div className="kv-row"><span className="k">buffer</span><span className="v">{agent?.headlines.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">emits per headline</span><span className="v">avg {total > 0 && agent ? (agent.totalEmitted / total).toFixed(2) : "—"}</span></div>
        <div className="kv-row"><span className="k">signaling</span><span className="v">via signal-bot</span></div>
      </div>
    </div>
  );
}
