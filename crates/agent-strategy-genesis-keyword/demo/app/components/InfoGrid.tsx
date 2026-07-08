"use client";

import type { GenesisState } from "@/lib/genesis-engine";

type Props = Readonly<{ agent: GenesisState | null }>;

export function InfoGrid({ agent }: Props) {
  const total = agent ? agent.totalCompiled + agent.totalErrored : 0;
  const errPct = total > 0 ? ((agent!.totalErrored / total) * 100).toFixed(1) + "%" : "—";
  const topAlgo = agent ? Object.entries(agent.byAlgo).sort((a, b) => b[1] - a[1]).slice(0, 3) : [];
  const topMarket = agent ? Object.entries(agent.byMarket).sort((a, b) => b[1] - a[1]).slice(0, 3) : [];

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">strategy-genesis</span></div>
        <div className="kv-row"><span className="k">compiler</span><span className="v mono" style={{ fontSize: 11 }}>keyword-v1</span></div>
        <div className="kv-row"><span className="k">output</span><span className="v">TOML</span></div>
        <div className="kv-row"><span className="k">algorithms</span><span className="v accent">3</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TRAFFIC</div></div>
        <div className="kv-row"><span className="k">specs seen</span><span className="v">{total}</span></div>
        <div className="kv-row"><span className="k">compiled</span><span className="v positive">{agent?.totalCompiled ?? 0}</span></div>
        <div className="kv-row"><span className="k">errored</span><span className={`v ${(agent?.totalErrored ?? 0) > 0 ? "negative" : ""}`}>{agent?.totalErrored ?? 0}</span></div>
        <div className="kv-row"><span className="k">error rate</span><span className="v">{errPct}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">ALGO PICKS</div></div>
        <div className="kv-row"><span className="k">twap</span><span className="v">{agent?.byAlgo.twap ?? 0}</span></div>
        <div className="kv-row"><span className="k">iceberg</span><span className="v">{agent?.byAlgo.iceberg ?? 0}</span></div>
        <div className="kv-row"><span className="k">liq-seek</span><span className="v">{agent?.byAlgo["liquidity-seeking"] ?? 0}</span></div>
        <div className="kv-row"><span className="k">top</span><span className="v accent">{topAlgo[0]?.[0] ?? "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">SIDE MIX</div></div>
        <div className="kv-row"><span className="k">buy</span><span className="v positive">{agent?.bySide.buy ?? 0}</span></div>
        <div className="kv-row"><span className="k">sell</span><span className="v negative">{agent?.bySide.sell ?? 0}</span></div>
        <div className="kv-row"><span className="k">markets</span><span className="v">{Object.keys(agent?.byMarket ?? {}).length}</span></div>
        <div className="kv-row"><span className="k">buffer</span><span className="v">{agent?.steps.length ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TOP MARKETS</div></div>
        {topMarket.length > 0 ? topMarket.map(([m, n]) => (
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
        <div className="kv-row"><span className="k">status</span><span className="v">ready</span></div>
        <div className="kv-row"><span className="k">last spec</span><span className="v mono" style={{ fontSize: 11 }}>{agent?.steps[agent.steps.length - 1] ? truncate(agent.steps[agent.steps.length - 1].spec, 22) : "—"}</span></div>
        <div className="kv-row"><span className="k">last algo</span><span className="v accent">{agent?.steps[agent.steps.length - 1]?.algorithm ?? "—"}</span></div>
        <div className="kv-row"><span className="k">consumes</span><span className="v">agent-algo-order</span></div>
      </div>
    </div>
  );
}

function truncate(s: string, n: number) { return s.length <= n ? s : s.slice(0, n - 1) + "…"; }
