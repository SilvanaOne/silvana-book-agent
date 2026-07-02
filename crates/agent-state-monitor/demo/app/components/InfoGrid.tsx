"use client";

import type { StateMonitorState, OrderEvent, SettlementEvent } from "@/lib/statemonitor-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

function fmtDuration(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return "0s";
  const s = Math.floor(ms / 1000);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m ${sec}s`;
  return `${sec}s`;
}

function summarizeOrder(e: OrderEvent): string {
  return `#${e.seq} ${e.kind} ${e.market} ${e.side} ${e.qty}@${fmt(e.price)}`;
}

function summarizeSettlement(e: SettlementEvent): string {
  return `#${e.seq} ${e.kind} ${e.market} notional=${fmt(e.notional)}`;
}

type Props = Readonly<{ statemonitor: StateMonitorState | null }>;

export function InfoGrid({ statemonitor }: Props) {
  const c = statemonitor?.config;
  const stats = statemonitor?.stats;
  const marketLabel = c ? (c.market && c.market !== "*" ? c.market : "* (all)") : "—";
  const subs: string[] = [];
  if (c?.includeOrders) subs.push("orders");
  if (c?.includeSettlements) subs.push("settlements");
  const subsLabel = subs.length ? subs.join(", ") : "—";
  const stateLabel = statemonitor === null ? "idle" : statemonitor.status === "monitoring" ? "streaming" : "stopped";

  const runtimeMs = statemonitor ? Date.now() - statemonitor.startedAt : 0;
  const runtimeMin = runtimeMs / 60_000;
  const totalEvents = (stats ? stats.ordersCreated + stats.ordersFilled + stats.ordersCancelled + stats.settlementsProposal + stats.settlementsSettled + stats.settlementsFailed : 0);
  const eventsPerMin = runtimeMin > 0.05 ? totalEvents / runtimeMin : 0;

  const recentOrders = statemonitor?.orderEvents.slice(-3).reverse() ?? [];
  const recentSettlements = statemonitor?.settlementEvents.slice(-3).reverse() ?? [];

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">state-monitor</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
        <div className="kv-row"><span className="k">running for</span><span className="v">{statemonitor ? fmtDuration(runtimeMs) : "—"}</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">SM CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">market filter</span><span className="v accent">{marketLabel}</span></div>
            <div className="kv-row"><span className="k">subscriptions</span><span className="v">{subsLabel}</span></div>
            <div className="kv-row"><span className="k">order arrival</span><span className="v">{c.orderArrivalPerTick}</span></div>
            <div className="kv-row"><span className="k">settlement arrival</span><span className="v">{c.settlementArrivalPerTick}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">market filter</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">subscriptions</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">order arrival</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">settlement arrival</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">ORDER STATS</div></div>
        <div className="kv-row"><span className="k">created</span><span className="v">{stats?.ordersCreated ?? 0}</span></div>
        <div className="kv-row"><span className="k">filled</span><span className="v accent">{stats?.ordersFilled ?? 0}</span></div>
        <div className="kv-row"><span className="k">cancelled</span><span className="v">{stats?.ordersCancelled ?? 0}</span></div>
        <div className="kv-row"><span className="k">current mid</span><span className="v">{fmt(statemonitor?.currentPrice)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">SETTLEMENT STATS</div></div>
        <div className="kv-row"><span className="k">proposal</span><span className="v">{stats?.settlementsProposal ?? 0}</span></div>
        <div className="kv-row"><span className="k">settled</span><span className="v accent">{stats?.settlementsSettled ?? 0}</span></div>
        <div className="kv-row"><span className="k">failed</span><span className={`v ${(stats?.settlementsFailed ?? 0) > 0 ? "negative" : ""}`}>{stats?.settlementsFailed ?? 0}</span></div>
        <div className="kv-row"><span className="k">ticks</span><span className="v">{statemonitor?.ticks ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RECENT</div></div>
        {recentOrders.length === 0 && recentSettlements.length === 0 && (
          <div className="kv-row"><span className="k">—</span><span className="v faint">no events yet</span></div>
        )}
        {recentOrders.map((e) => (
          <div className="kv-row" key={`o-${e.seq}`}><span className="k">order</span><span className="v mono" style={{ fontSize: 11 }}>{summarizeOrder(e)}</span></div>
        ))}
        {recentSettlements.map((e) => (
          <div className="kv-row" key={`s-${e.seq}`}><span className="k">settlement</span><span className="v mono" style={{ fontSize: 11 }}>{summarizeSettlement(e)}</span></div>
        ))}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">THROUGHPUT</div></div>
        <div className="kv-row"><span className="k">events / min</span><span className="v accent">{eventsPerMin.toFixed(1)}</span></div>
        <div className="kv-row"><span className="k">events total</span><span className="v">{totalEvents}</span></div>
        <div className="kv-row"><span className="k">buffered orders</span><span className="v">{statemonitor?.orderEvents.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">buffered settls</span><span className="v">{statemonitor?.settlementEvents.length ?? 0}</span></div>
      </div>
    </div>
  );
}
