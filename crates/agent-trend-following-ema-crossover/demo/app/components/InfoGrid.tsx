"use client";

import type { TrendFollowingState } from "@/lib/trendfollowing-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

type Props = Readonly<{ trendfollowing: TrendFollowingState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ trendfollowing, walk }: Props) {
  const c = trendfollowing?.config;
  const inWarmup = trendfollowing && trendfollowing.samples < (c?.warmupSamples ?? 0);
  const openOrders = trendfollowing?.orders.filter((o) => o.status === "open").length ?? 0;
  const stateLabel = trendfollowing === null ? "idle" : inWarmup ? "warmup" : trendfollowing.status === "monitoring" ? "armed" : "stopped";
  const pnlClass = (trendfollowing?.realizedPnl ?? 0) > 0 ? "positive" : (trendfollowing?.realizedPnl ?? 0) < 0 ? "negative" : "";

  const dirLabel = !trendfollowing || trendfollowing.emaFast === null || trendfollowing.emaSlow === null
    ? "—"
    : trendfollowing.emaFast > trendfollowing.emaSlow ? "bullish (fast > slow)"
    : trendfollowing.emaFast < trendfollowing.emaSlow ? "bearish (fast < slow)"
    : "flat";
  const dirClass = !trendfollowing || trendfollowing.emaFast === null || trendfollowing.emaSlow === null ? ""
    : trendfollowing.emaFast > trendfollowing.emaSlow ? "positive"
    : trendfollowing.emaFast < trendfollowing.emaSlow ? "negative"
    : "";
  const lastCrossLabel = !trendfollowing || trendfollowing.lastCross === "flat" ? "—"
    : trendfollowing.lastCross === "up" ? "bullish (BID)"
    : "bearish (OFFER)";

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">trend-following</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TF CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v">{c.market}</span></div>
            <div className="kv-row"><span className="k">fast window</span><span className="v accent">{c.fastWindow}</span></div>
            <div className="kv-row"><span className="k">slow window</span><span className="v">{c.slowWindow}</span></div>
            <div className="kv-row"><span className="k">quantity</span><span className="v">{c.quantity}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">fast window</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">slow window</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">quantity</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LIVE STATE</div></div>
        <div className="kv-row"><span className="k">current mid</span><span className="v">{fmt(trendfollowing?.currentPrice)}</span></div>
        <div className="kv-row"><span className="k">fast ema</span><span className="v accent">{fmt(trendfollowing?.emaFast ?? null)}</span></div>
        <div className="kv-row"><span className="k">slow ema</span><span className="v">{fmt(trendfollowing?.emaSlow ?? null)}</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">EMA STATE</div></div>
        <div className="kv-row"><span className="k">alpha fast</span><span className="v">{trendfollowing ? trendfollowing.alphaFast.toFixed(4) : "—"}</span></div>
        <div className="kv-row"><span className="k">alpha slow</span><span className="v">{trendfollowing ? trendfollowing.alphaSlow.toFixed(4) : "—"}</span></div>
        <div className="kv-row"><span className="k">samples</span><span className="v">{trendfollowing?.samples ?? 0}</span></div>
        <div className="kv-row"><span className="k">warmup</span><span className="v">{c ? c.warmupSamples : "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">SIGNALS</div></div>
        <div className="kv-row"><span className="k">signals emitted</span><span className="v">{trendfollowing?.signals ?? 0}</span></div>
        <div className="kv-row"><span className="k">orders placed</span><span className="v">{trendfollowing?.ordersPlaced ?? 0}</span></div>
        <div className="kv-row"><span className="k">orders filled</span><span className="v accent">{trendfollowing?.ordersFilled ?? 0}</span></div>
        <div className="kv-row"><span className="k">open orders</span><span className="v">{openOrders}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">P&L (SIM)</div></div>
        <div className="kv-row"><span className="k">realized pnl</span><span className={`v ${pnlClass}`}>{fmt(trendfollowing?.realizedPnl)}</span></div>
        <div className="kv-row"><span className="k">direction</span><span className={`v ${dirClass}`}>{dirLabel}</span></div>
        <div className="kv-row"><span className="k">last cross</span><span className="v">{lastCrossLabel}</span></div>
        <div className="kv-row"><span className="k">walk source</span><span className="v">GBM drift {walk.driftPerTick} · vol {(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>
    </div>
  );
}
