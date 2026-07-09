"use client";

import type { SpotGridState } from "@/lib/spotgrid-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

function tsFmt(t: number | null | undefined): string {
  if (!t) return "—";
  const d = new Date(t);
  return d.toISOString().slice(11, 19);
}

type Props = Readonly<{ spotgrid: SpotGridState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ spotgrid, walk }: Props) {
  const c = spotgrid?.config;
  const orders = spotgrid?.orders ?? [];
  const openBids = orders.filter((o) => o.status === "open" && o.side === "BID").length;
  const openOffers = orders.filter((o) => o.status === "open" && o.side === "OFFER").length;
  const bidFills = spotgrid?.bidFills ?? 0;
  const offerFills = spotgrid?.offerFills ?? 0;
  const netQty = orders
    .filter((o) => o.status === "filled")
    .reduce((sum, o) => sum + (o.side === "BID" ? o.qty : -o.qty), 0);
  const notional = orders
    .filter((o) => o.status === "filled")
    .reduce((sum, o) => sum + o.qty * o.price, 0);
  const stateLabel = spotgrid === null ? "idle" : spotgrid.status === "monitoring" ? "monitoring" : "stopped";

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">spot-grid</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">GRID CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v">{c.market}</span></div>
            <div className="kv-row"><span className="k">mid</span><span className="v accent">{fmt(c.midPrice)}</span></div>
            <div className="kv-row"><span className="k">levels</span><span className="v">{c.bidLevels} bid · {c.offerLevels} offer</span></div>
            <div className="kv-row"><span className="k">base step</span><span className="v">{c.baseStepPct}%</span></div>
            <div className="kv-row"><span className="k">ratio</span><span className="v">×{c.ratio.toFixed(2)}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">mid</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">levels</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">base step</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">ratio</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LIVE STATE</div></div>
        <div className="kv-row"><span className="k">current mid</span><span className="v">{fmt(spotgrid?.currentPrice)}</span></div>
        <div className="kv-row"><span className="k">open bids</span><span className="v">{openBids}</span></div>
        <div className="kv-row"><span className="k">open offers</span><span className="v">{openOffers}</span></div>
        <div className="kv-row"><span className="k">qty / level</span><span className="v">{c ? c.qtyPerLevel : "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">FILLS</div></div>
        <div className="kv-row"><span className="k">bid fills</span><span className="v positive">{bidFills}</span></div>
        <div className="kv-row"><span className="k">offer fills</span><span className="v negative">{offerFills}</span></div>
        <div className="kv-row"><span className="k">last fill</span><span className="v accent">{tsFmt(spotgrid?.lastFillAt)}</span></div>
        <div className="kv-row"><span className="k">last event</span><span className="v mono" style={{ fontSize: 11 }}>{spotgrid?.lastFillMsg ?? "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">INVENTORY</div></div>
        <div className="kv-row"><span className="k">net qty (bid−offer)</span><span className={`v ${netQty > 0 ? "positive" : netQty < 0 ? "negative" : ""}`}>{netQty.toFixed(4)}</span></div>
        <div className="kv-row"><span className="k">notional filled</span><span className="v">{fmt(notional)}</span></div>
        <div className="kv-row"><span className="k">bid fills / total</span><span className="v">{bidFills} / {c?.bidLevels ?? 0}</span></div>
        <div className="kv-row"><span className="k">offer fills / total</span><span className="v">{offerFills} / {c?.offerLevels ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PRICE SIM</div></div>
        <div className="kv-row"><span className="k">source</span><span className="v">GBM walk</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
        <div className="kv-row"><span className="k">restore filled</span><span className="v">off</span></div>
      </div>
    </div>
  );
}
