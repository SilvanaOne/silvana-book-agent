"use client";

import type { ObStreamState } from "@/lib/obstream-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

type Props = Readonly<{ agent: ObStreamState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ agent, walk }: Props) {
  const c = agent?.config;
  const streams = c
    ? [c.noPrices ? null : "prices", c.noDepth ? null : "depth"].filter((s): s is string => s !== null).join(",") || "—"
    : "—";
  const total = (agent?.totalDelivered ?? 0) + (agent?.totalFailed ?? 0);
  const successRate = total > 0 ? ((agent!.totalDelivered / total) * 100).toFixed(1) + "%" : "—";
  const rateClass = agent && total > 0 && agent.totalFailed / total > 0.1 ? "negative" : "";
  const stateLabel = agent === null ? "idle" : agent.status === "streaming" ? "streaming" : "stopped";
  const primary = agent?.books[0];

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">orderbook-streaming</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STREAM CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">streams</span><span className="v accent">{streams}</span></div>
            <div className="kv-row"><span className="k">markets</span><span className="v">{c.markets.length}</span></div>
            <div className="kv-row"><span className="k">depth levels</span><span className="v">{c.depth}</span></div>
            <div className="kv-row"><span className="k">source</span><span className="v mono" style={{ fontSize: 11 }}>{c.source}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">streams</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">markets</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">depth levels</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">source</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TRAFFIC</div></div>
        <div className="kv-row"><span className="k">events total</span><span className="v">{agent?.totalEvents ?? 0}</span></div>
        <div className="kv-row"><span className="k">events / min</span><span className="v">{agent?.eventsPerMinute ?? 0}</span></div>
        <div className="kv-row"><span className="k">price ticks</span><span className="v">{agent?.priceEvents ?? 0}</span></div>
        <div className="kv-row"><span className="k">depth updates</span><span className="v">{(agent?.snapshotEvents ?? 0) + (agent?.deltaEvents ?? 0)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">DELIVERY</div></div>
        <div className="kv-row"><span className="k">delivered</span><span className="v positive">{agent?.totalDelivered ?? 0}</span></div>
        <div className="kv-row"><span className="k">failed</span><span className={`v ${(agent?.totalFailed ?? 0) > 0 ? "negative" : ""}`}>{agent?.totalFailed ?? 0}</span></div>
        <div className="kv-row"><span className="k">success rate</span><span className={`v ${rateClass}`}>{successRate}</span></div>
        <div className="kv-row"><span className="k">attempts</span><span className="v">{total}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TOP OF BOOK</div></div>
        {primary ? (
          <>
            <div className="kv-row"><span className="k mono" style={{ fontSize: 11 }}>{primary.market}</span><span className="v mono" style={{ fontSize: 11 }}>{fmt(primary.price)}</span></div>
            <div className="kv-row"><span className="k">best bid</span><span className="v positive">{fmt(primary.bestBid)}</span></div>
            <div className="kv-row"><span className="k">best ask</span><span className="v negative">{fmt(primary.bestAsk)}</span></div>
            <div className="kv-row"><span className="k">last kind</span><span className="v accent mono" style={{ fontSize: 11 }}>{primary.lastEventKind ?? "—"}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">best bid</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">best ask</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">last kind</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATE</div></div>
        <div className="kv-row"><span className="k">status</span><span className="v">{stateLabel}</span></div>
        <div className="kv-row"><span className="k">source</span><span className="v">GBM walk</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>
    </div>
  );
}
