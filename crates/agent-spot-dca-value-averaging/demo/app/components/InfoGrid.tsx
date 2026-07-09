"use client";

import type { DcaState } from "@/lib/dca-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

type Props = Readonly<{ dca: DcaState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ dca, walk }: Props) {
  const c = dca?.config;
  const progress =
    c?.maxTotalQuote && dca && dca.placedNotional > 0
      ? Math.min(100, (dca.placedNotional / c.maxTotalQuote) * 100)
      : null;
  const trackingErr =
    dca && dca.cycle > 0
      ? dca.placedNotional - dca.targetCumulative
      : null;

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">dca-value-averaging</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">VA CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v">{c.market}</span></div>
            <div className="kv-row"><span className="k">side</span><span className={`v ${c.side === "buy" ? "accent" : ""}`}>{c.side.toUpperCase()}</span></div>
            <div className="kv-row"><span className="k">value / period</span><span className="v">{c.valuePerPeriod}</span></div>
            <div className="kv-row"><span className="k">interval</span><span className="v">{c.intervalSecs} s</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">side</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">value / period</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">interval</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LIVE STATE</div></div>
        <div className="kv-row"><span className="k">current price</span><span className="v">{fmt(dca?.currentPrice)}</span></div>
        <div className="kv-row"><span className="k">cycles</span><span className="v">{dca?.cycle ?? 0}</span></div>
        <div className="kv-row"><span className="k">orders placed</span><span className="v">{dca?.orderCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">total accumulated</span><span className="v accent">{fmt(dca?.totalQty)}</span></div>
        <div className="kv-row"><span className="k">avg price</span><span className="v accent">{fmt(dca?.avgPrice)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">SCHEDULE</div></div>
        <div className="kv-row"><span className="k">target cumulative</span><span className="v">{fmt(dca?.targetCumulative)}</span></div>
        <div className="kv-row"><span className="k">placed notional</span><span className="v">{fmt(dca?.placedNotional)}</span></div>
        <div className="kv-row"><span className="k">tracking Δ</span><span className={`v ${trackingErr && trackingErr < 0 ? "negative" : "positive"}`}>{fmt(trackingErr)}</span></div>
        <div className="kv-row"><span className="k">max total quote</span><span className="v">{c?.maxTotalQuote ?? "∞"}</span></div>
        <div className="kv-row"><span className="k">completed</span><span className="v">{progress === null ? "—" : `${progress.toFixed(1)}%`}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PRICING</div></div>
        <div className="kv-row"><span className="k">offset from mid</span><span className="v">{c ? `${c.priceOffsetPct >= 0 ? "+" : ""}${c.priceOffsetPct}%` : "—"}</span></div>
        <div className="kv-row"><span className="k">max order quote</span><span className="v">{c?.maxOrderQuote ?? "∞"}</span></div>
        <div className="kv-row"><span className="k">starting mid</span><span className="v">{fmt(c?.startingPrice)}</span></div>
        <div className="kv-row"><span className="k">last order price</span><span className="v">{fmt(dca?.orders[dca.orders.length - 1]?.price)}</span></div>
        <div className="kv-row"><span className="k">price delta vs entry</span><span className="v">{c && dca?.currentPrice ? `${(((dca.currentPrice - c.startingPrice) / c.startingPrice) * 100).toFixed(2)}%` : "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PRICE SIM</div></div>
        <div className="kv-row"><span className="k">source</span><span className="v">GBM walk</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
        <div className="kv-row"><span className="k">status</span><span className="v">{dca?.status === "monitoring" ? "streaming" : "paused"}</span></div>
      </div>
    </div>
  );
}
