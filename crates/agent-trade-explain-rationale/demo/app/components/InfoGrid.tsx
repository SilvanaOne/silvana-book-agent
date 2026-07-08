"use client";

import type { ExplainState } from "@/lib/explain-engine";

type Props = Readonly<{ agent: ExplainState | null }>;

export function InfoGrid({ agent }: Props) {
  const c = agent?.config;
  const topMarket = agent ? Object.entries(agent.byMarket).sort((a, b) => b[1] - a[1]).slice(0, 3) : [];

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">trade-explain</span></div>
        <div className="kv-row"><span className="k">model</span><span className="v mono" style={{ fontSize: 11 }}>{c?.model ?? "—"}</span></div>
        <div className="kv-row"><span className="k">runs</span><span className="v">{agent?.runs ?? 0}</span></div>
        <div className="kv-row"><span className="k">output</span><span className="v">signed JSONL</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">CONFIG</div></div>
        <div className="kv-row"><span className="k">history size</span><span className="v">{c?.historySize ?? 0}</span></div>
        <div className="kv-row"><span className="k">markets</span><span className="v">{c?.markets.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">include orders</span><span className="v accent">{c?.includeOrders ? "yes" : "no"}</span></div>
        <div className="kv-row"><span className="k">include settle</span><span className="v accent">{c?.includeSettlements ? "yes" : "no"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TRAFFIC</div></div>
        <div className="kv-row"><span className="k">history seen</span><span className="v">{agent?.totalHistory ?? 0}</span></div>
        <div className="kv-row"><span className="k">explained</span><span className="v positive">{agent?.totalExplained ?? 0}</span></div>
        <div className="kv-row"><span className="k">order rationale</span><span className="v">{agent?.byKind["order.created"] ?? 0}</span></div>
        <div className="kv-row"><span className="k">settle rationale</span><span className="v">{agent?.byKind["settlement.settled"] ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">SIDE MIX</div></div>
        <div className="kv-row"><span className="k">BID</span><span className="v positive">{agent?.bySide.BID ?? 0}</span></div>
        <div className="kv-row"><span className="k">OFFER</span><span className="v negative">{agent?.bySide.OFFER ?? 0}</span></div>
        <div className="kv-row"><span className="k">unique markets</span><span className="v">{Object.keys(agent?.byMarket ?? {}).length}</span></div>
        <div className="kv-row"><span className="k">buffer</span><span className="v">{agent?.explanations.length ?? 0}</span></div>
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
        <div className="kv-row"><span className="k">status</span><span className="v">{agent ? "loaded" : "idle"}</span></div>
        <div className="kv-row"><span className="k">avg pieces / rec</span><span className="v">2 (rationale + counterfactual)</span></div>
        <div className="kv-row"><span className="k">flavor</span><span className="v">natural-language</span></div>
        <div className="kv-row"><span className="k">signed</span><span className="v">ed25519</span></div>
      </div>
    </div>
  );
}
