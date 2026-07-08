"use client";

import type { AiSignalState } from "@/lib/ai-signal-engine";

type Props = Readonly<{ agent: AiSignalState | null }>;

export function InfoGrid({ agent }: Props) {
  const c = agent?.config;
  const total = agent ? agent.totalEmitted + agent.totalDropped : 0;
  const keepPct = total > 0 ? ((agent!.totalEmitted / total) * 100).toFixed(1) + "%" : "—";
  const topMarkets = agent ? Object.entries(agent.byMarket).sort((a, b) => b[1] - a[1]).slice(0, 3) : [];

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">ai-signal</span></div>
        <div className="kv-row"><span className="k">model</span><span className="v mono" style={{ fontSize: 11 }}>{c?.model ?? "—"}</span></div>
        <div className="kv-row"><span className="k">tick interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">auto emit</span><span className="v accent">{c?.autoPromptEverySec === 0 ? "manual" : c ? c.autoPromptEverySec + "s" : "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">CONFIG</div></div>
        <div className="kv-row"><span className="k">markets</span><span className="v">{c?.markets.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">min conf</span><span className="v">{c ? (c.minConfidence * 100).toFixed(0) + "%" : "—"}</span></div>
        <div className="kv-row"><span className="k">prompt pool</span><span className="v">{c?.prompts.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">first mid</span><span className="v">{c ? c.mids[0].toFixed(4) : "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">SIGNALS</div></div>
        <div className="kv-row"><span className="k">total prompts</span><span className="v">{total}</span></div>
        <div className="kv-row"><span className="k">emitted</span><span className="v positive">{agent?.totalEmitted ?? 0}</span></div>
        <div className="kv-row"><span className="k">dropped &lt; min</span><span className={`v ${(agent?.totalDropped ?? 0) > 0 ? "negative" : ""}`}>{agent?.totalDropped ?? 0}</span></div>
        <div className="kv-row"><span className="k">keep rate</span><span className="v">{keepPct}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">DIRECTION</div></div>
        <div className="kv-row"><span className="k">BID</span><span className="v positive">{agent?.bySide.BID ?? 0}</span></div>
        <div className="kv-row"><span className="k">OFFER</span><span className="v negative">{agent?.bySide.OFFER ?? 0}</span></div>
        <div className="kv-row"><span className="k">avg conf</span><span className="v">{agent && agent.totalEmitted > 0 ? (agent.avgConfidence * 100).toFixed(1) + "%" : "—"}</span></div>
        <div className="kv-row"><span className="k">last kind</span><span className="v mono" style={{ fontSize: 11 }}>{agent?.signals[agent.signals.length - 1]?.dropped ? "drop" : agent?.signals.length ? "signal" : "—"}</span></div>
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
        <div className="kv-row"><span className="k">status</span><span className="v">{agent === null ? "idle" : agent.status === "running" ? "predicting" : "stopped"}</span></div>
        <div className="kv-row"><span className="k">buffer</span><span className="v">{agent?.signals.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">next auto</span><span className="v">{agent?.nextEmitAt ? Math.max(0, Math.round((agent.nextEmitAt - Date.now()) / 1000)) + "s" : "—"}</span></div>
        <div className="kv-row"><span className="k">signaling</span><span className="v">via signal-bot</span></div>
      </div>
    </div>
  );
}
