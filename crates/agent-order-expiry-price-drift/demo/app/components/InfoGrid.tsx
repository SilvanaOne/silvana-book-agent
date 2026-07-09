"use client";

import type { OrderExpiryState } from "@/lib/orderexpiry-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

function secs(n: number | null | undefined): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  return `${Math.max(0, Math.round(n))}s`;
}

function pct(n: number | null | undefined): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  return `${n.toFixed(2)}%`;
}

type Props = Readonly<{ orderexpiry: OrderExpiryState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ orderexpiry, walk }: Props) {
  const c = orderexpiry?.config;
  const now = Date.now();
  const activeOrders = orderexpiry?.orders.filter((o) => o.status === "active") ?? [];
  const mid = orderexpiry?.currentPrice ?? 0;
  const maxCurrentDrift = mid > 0 && activeOrders.length > 0
    ? Math.max(...activeOrders.map((o) => Math.abs(o.price - mid) / mid * 100))
    : 0;
  const nextCheckIn = orderexpiry && c
    ? Math.max(0, c.checkIntervalSecs - (now - (orderexpiry.lastCheckAt ?? orderexpiry.startedAt)) / 1000)
    : 0;
  const uptimeSecs = orderexpiry ? Math.floor((now - orderexpiry.startedAt) / 1000) : 0;
  const avgDriftAtCancel = orderexpiry && orderexpiry.cancelledCount > 0
    ? orderexpiry.sumDriftAtCancel / orderexpiry.cancelledCount
    : null;
  const stateLabel = orderexpiry === null ? "idle" : orderexpiry.status === "monitoring" ? "running" : "stopped";
  const overThreshold = c && maxCurrentDrift > c.maxDriftPct;

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">order-expiry-price-drift</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">OE CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v">{c.market}</span></div>
            <div className="kv-row"><span className="k">max drift</span><span className="v accent">{c.maxDriftPct}%</span></div>
            <div className="kv-row"><span className="k">check interval</span><span className="v">{c.checkIntervalSecs}s</span></div>
            <div className="kv-row"><span className="k">dry-run</span><span className={`v ${c.dryRun ? "accent" : ""}`}>{c.dryRun ? "yes" : "no"}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">max drift</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">check interval</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">dry-run</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LIVE STATE</div></div>
        <div className="kv-row"><span className="k">open orders</span><span className="v">{activeOrders.length}</span></div>
        <div className="kv-row"><span className="k">worst drift now</span><span className={`v ${overThreshold ? "accent" : ""}`}>{pct(maxCurrentDrift)}</span></div>
        <div className="kv-row"><span className="k">next sweep in</span><span className="v">{secs(nextCheckIn)}</span></div>
        <div className="kv-row"><span className="k">last sweep at</span><span className="v">{orderexpiry?.lastCheckAt ? new Date(orderexpiry.lastCheckAt).toLocaleTimeString() : "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">THROUGHPUT</div></div>
        <div className="kv-row"><span className="k">new orders arrived</span><span className="v">{orderexpiry?.arrivedCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">cancels performed</span><span className="v accent">{orderexpiry?.cancelledCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">sweeps run</span><span className="v">{orderexpiry?.checksCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">uptime</span><span className="v">{secs(uptimeSecs)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATS</div></div>
        <div className="kv-row"><span className="k">avg drift at cancel</span><span className="v">{avgDriftAtCancel !== null ? pct(avgDriftAtCancel) : "—"}</span></div>
        <div className="kv-row"><span className="k">max drift ever cancelled</span><span className="v">{orderexpiry ? pct(orderexpiry.maxDriftEverPct) : "—"}</span></div>
        <div className="kv-row"><span className="k">last cancel reason</span><span className="v mono" style={{ fontSize: 11 }}>{orderexpiry?.lastCancelReason ?? "—"}</span></div>
        <div className="kv-row"><span className="k">arrival rate</span><span className="v">{c ? `λ=${c.orderArrivalPerTick}/tick` : "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PRICE SIM</div></div>
        <div className="kv-row"><span className="k">current mid</span><span className="v">{fmt(orderexpiry?.currentPrice)}</span></div>
        <div className="kv-row"><span className="k">source</span><span className="v">GBM walk</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>
    </div>
  );
}
