"use client";

import type { SpreadCaptureState } from "@/lib/spreadcapture-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

type Props = Readonly<{ spreadcapture: SpreadCaptureState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ spreadcapture, walk }: Props) {
  const c = spreadcapture?.config;
  const openOrders = spreadcapture?.orders.filter((o) => o.status === "open").length ?? 0;
  const pnlClass = (spreadcapture?.realizedPnl ?? 0) > 0 ? "positive" : (spreadcapture?.realizedPnl ?? 0) < 0 ? "negative" : "";

  const now = Date.now();
  const nextRefreshIn = spreadcapture && c
    ? Math.max(0, Math.ceil((spreadcapture.lastRefreshAt + c.refreshSecs * 1000 - now) / 1000))
    : null;
  const stateLabel = spreadcapture === null
    ? "idle"
    : spreadcapture.status === "monitoring"
      ? "quoting"
      : "stopped";

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">spread-capture</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">SC CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v">{c.market}</span></div>
            <div className="kv-row"><span className="k">spread</span><span className="v accent">±{c.spreadBps} bps</span></div>
            <div className="kv-row"><span className="k">quantity</span><span className="v">{c.quantity}</span></div>
            <div className="kv-row"><span className="k">max inventory</span><span className="v">±{c.maxInventory}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">spread</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">quantity</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">max inventory</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LIVE STATE</div></div>
        <div className="kv-row"><span className="k">current mid</span><span className="v">{fmt(spreadcapture?.currentPrice)}</span></div>
        <div className="kv-row"><span className="k">bid price</span><span className="v positive">{spreadcapture?.bidActive ? fmt(spreadcapture.bidActive.price) : "—"}</span></div>
        <div className="kv-row"><span className="k">offer price</span><span className="v negative">{spreadcapture?.offerActive ? fmt(spreadcapture.offerActive.price) : "—"}</span></div>
        <div className="kv-row"><span className="k">inventory</span><span className="v accent">{spreadcapture ? (spreadcapture.netInventory >= 0 ? "+" : "") + spreadcapture.netInventory.toFixed(4) : "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">CYCLE</div></div>
        <div className="kv-row"><span className="k">cycles refreshed</span><span className="v">{spreadcapture?.cyclesRefreshed ?? 0}</span></div>
        <div className="kv-row"><span className="k">next refresh in</span><span className="v">{nextRefreshIn === null ? "—" : `${nextRefreshIn}s`}</span></div>
        <div className="kv-row"><span className="k">orders placed</span><span className="v">{spreadcapture?.ordersPlaced ?? 0}</span></div>
        <div className="kv-row"><span className="k">orders filled</span><span className="v accent">{spreadcapture?.ordersFilled ?? 0}</span></div>
        <div className="kv-row"><span className="k">open orders</span><span className="v">{openOrders}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">P&L</div></div>
        <div className="kv-row"><span className="k">realized pnl</span><span className={`v ${pnlClass}`}>{fmt(spreadcapture?.realizedPnl)}</span></div>
        <div className="kv-row"><span className="k">spread captured Σ</span><span className="v accent">{fmt(spreadcapture?.spreadCapturedCum)}</span></div>
        <div className="kv-row"><span className="k">avg captured bps</span><span className="v">{spreadcapture && spreadcapture.spreadSamples > 0 ? spreadcapture.avgSpreadBps.toFixed(1) : "—"}</span></div>
        <div className="kv-row"><span className="k">paired fills</span><span className="v">{spreadcapture?.spreadSamples ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PRICE SIM</div></div>
        <div className="kv-row"><span className="k">source</span><span className="v">GBM walk</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
        <div className="kv-row"><span className="k">tick spacing</span><span className="v">1000 ms</span></div>
      </div>
    </div>
  );
}
