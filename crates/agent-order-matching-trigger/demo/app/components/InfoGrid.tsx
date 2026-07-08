"use client";

import type { OrderMatchingState } from "@/lib/ordermatching-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

function bps(n: number | null | undefined): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const s = n >= 0 ? "+" : "";
  return `${s}${n.toFixed(1)} bps`;
}

type Props = Readonly<{ ordermatching: OrderMatchingState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ ordermatching, walk }: Props) {
  const c = ordermatching?.config;
  const stateLabel = ordermatching === null ? "idle" : ordermatching.status === "monitoring" ? "armed" : "stopped";

  const bestBid = ordermatching?.bestBid ?? null;
  const bestOffer = ordermatching?.bestOffer ?? null;
  const mid = ordermatching?.currentPrice ?? null;
  const spreadBps = bestBid !== null && bestOffer !== null && mid && mid > 0
    ? ((bestOffer - bestBid) / mid) * 10000
    : null;

  const buyArmed = c && c.buyTrigger !== null;
  const sellArmed = c && c.sellTrigger !== null;
  const distBuyBps = buyArmed && bestOffer !== null && c!.buyTrigger !== null
    ? ((bestOffer - c!.buyTrigger) / c!.buyTrigger) * 10000
    : null;
  const distSellBps = sellArmed && bestBid !== null && c!.sellTrigger !== null
    ? ((bestBid - c!.sellTrigger) / c!.sellTrigger) * 10000
    : null;

  const openBuy = ordermatching?.buySnipeOpen !== null && ordermatching?.buySnipeOpen !== undefined;
  const openSell = ordermatching?.sellSnipeOpen !== null && ordermatching?.sellSnipeOpen !== undefined;
  const pnlClass = (ordermatching?.realizedPnl ?? 0) > 0 ? "positive" : (ordermatching?.realizedPnl ?? 0) < 0 ? "negative" : "";

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">order-matching</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">OM CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v">{c.market}</span></div>
            <div className="kv-row"><span className="k">buy trigger</span><span className="v accent">{c.buyTrigger !== null ? `≤ ${fmt(c.buyTrigger)}` : "off"}</span></div>
            <div className="kv-row"><span className="k">sell trigger</span><span className="v accent">{c.sellTrigger !== null ? `≥ ${fmt(c.sellTrigger)}` : "off"}</span></div>
            <div className="kv-row"><span className="k">quantity</span><span className="v">{c.quantity}</span></div>
            <div className="kv-row"><span className="k">book spread</span><span className="v">{c.bookSpreadBps} bps</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">buy trigger</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">sell trigger</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">quantity</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">book spread</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LIVE BOOK</div></div>
        <div className="kv-row"><span className="k">mid</span><span className="v">{fmt(mid)}</span></div>
        <div className="kv-row"><span className="k">best bid</span><span className="v positive">{fmt(bestBid)}</span></div>
        <div className="kv-row"><span className="k">best offer</span><span className="v negative">{fmt(bestOffer)}</span></div>
        <div className="kv-row"><span className="k">spread</span><span className="v">{bps(spreadBps)}</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TRIGGERS</div></div>
        <div className="kv-row"><span className="k">buy trigger</span><span className="v">{buyArmed ? "armed" : "off"}</span></div>
        <div className="kv-row"><span className="k">sell trigger</span><span className="v">{sellArmed ? "armed" : "off"}</span></div>
        <div className="kv-row"><span className="k">best_offer − buy</span><span className={`v ${distBuyBps !== null && distBuyBps <= 0 ? "accent" : ""}`}>{bps(distBuyBps)}</span></div>
        <div className="kv-row"><span className="k">best_bid − sell</span><span className={`v ${distSellBps !== null && distSellBps >= 0 ? "accent" : ""}`}>{bps(distSellBps)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">SNIPES</div></div>
        <div className="kv-row"><span className="k">open BID snipe</span><span className="v">{openBuy ? `#${ordermatching!.buySnipeOpen!.seq} @ ${fmt(ordermatching!.buySnipeOpen!.price)}` : "—"}</span></div>
        <div className="kv-row"><span className="k">open OFFER snipe</span><span className="v">{openSell ? `#${ordermatching!.sellSnipeOpen!.seq} @ ${fmt(ordermatching!.sellSnipeOpen!.price)}` : "—"}</span></div>
        <div className="kv-row"><span className="k">snipes placed</span><span className="v">{ordermatching?.snipesPlaced ?? 0}</span></div>
        <div className="kv-row"><span className="k">snipes filled</span><span className="v accent">{ordermatching?.snipesFilled ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATS</div></div>
        <div className="kv-row"><span className="k">realized pnl (sim)</span><span className={`v ${pnlClass}`}>{fmt(ordermatching?.realizedPnl)}</span></div>
        <div className="kv-row"><span className="k">source</span><span className="v">GBM walk</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>
    </div>
  );
}
