"use client";

import type { TrendFollowingState } from "@/lib/trendfollowing-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

function fmtSigned(n: number | null | undefined): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  return (n >= 0 ? "+" : "") + fmt(n);
}

function timeAgo(t: number | null): string {
  if (t === null) return "—";
  const s = Math.max(0, Math.floor((Date.now() - t) / 1000));
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  return `${Math.floor(s / 3600)}h ago`;
}

type Props = Readonly<{ trendfollowing: TrendFollowingState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ trendfollowing, walk }: Props) {
  const c = trendfollowing?.config;
  const inWarmup = trendfollowing && trendfollowing.samples < (c?.warmupSamples ?? 0);
  const openOrders = trendfollowing?.orders.filter((o) => o.status === "open").length ?? 0;
  const stateLabel = trendfollowing === null ? "idle" : inWarmup ? "warmup" : trendfollowing.status === "monitoring" ? "armed" : "stopped";
  const pnlClass = (trendfollowing?.realizedPnl ?? 0) > 0 ? "positive" : (trendfollowing?.realizedPnl ?? 0) < 0 ? "negative" : "";

  const hist = trendfollowing?.histogram ?? null;
  const histClass = hist === null ? "" : hist > 0 ? "positive" : hist < 0 ? "negative" : "";
  const dirLabel = hist === null ? "—" : hist > 0 ? "bullish (MACD > signal)" : hist < 0 ? "bearish (MACD < signal)" : "flat";
  const lastCrossLabel = !trendfollowing || trendfollowing.lastCross === "flat" ? "—"
    : trendfollowing.lastCross === "up" ? "bullish (BID)"
    : "bearish (OFFER)";

  const warmupPct = c && c.warmupSamples > 0 && trendfollowing
    ? Math.min(100, Math.round((trendfollowing.samples / c.warmupSamples) * 100))
    : 0;
  const warmupText = c ? `${trendfollowing?.samples ?? 0} / ${c.warmupSamples} (${warmupPct}%)` : "—";

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">trend-following-macd</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">MACD CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v">{c.market}</span></div>
            <div className="kv-row"><span className="k">fast window</span><span className="v accent">{c.fastWindow}</span></div>
            <div className="kv-row"><span className="k">slow window</span><span className="v">{c.slowWindow}</span></div>
            <div className="kv-row"><span className="k">signal period</span><span className="v">{c.signalPeriod}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">fast window</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">slow window</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">signal period</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LIVE STATE</div></div>
        <div className="kv-row"><span className="k">current mid</span><span className="v">{fmt(trendfollowing?.currentPrice)}</span></div>
        <div className="kv-row"><span className="k">ema fast</span><span className="v accent">{fmt(trendfollowing?.emaFast ?? null)}</span></div>
        <div className="kv-row"><span className="k">ema slow</span><span className="v">{fmt(trendfollowing?.emaSlow ?? null)}</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">MACD</div></div>
        <div className="kv-row"><span className="k">macd</span><span className="v accent">{fmtSigned(trendfollowing?.macd ?? null)}</span></div>
        <div className="kv-row"><span className="k">signal</span><span className="v">{fmtSigned(trendfollowing?.signal ?? null)}</span></div>
        <div className="kv-row"><span className="k">histogram</span><span className={`v ${histClass}`}>{fmtSigned(hist)}</span></div>
        <div className="kv-row"><span className="k">warmup</span><span className="v">{warmupText}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">EMA STATE</div></div>
        <div className="kv-row"><span className="k">alpha fast</span><span className="v">{trendfollowing ? trendfollowing.alphaFast.toFixed(4) : "—"}</span></div>
        <div className="kv-row"><span className="k">alpha slow</span><span className="v">{trendfollowing ? trendfollowing.alphaSlow.toFixed(4) : "—"}</span></div>
        <div className="kv-row"><span className="k">alpha signal</span><span className="v">{trendfollowing ? trendfollowing.alphaSignal.toFixed(4) : "—"}</span></div>
        <div className="kv-row"><span className="k">samples</span><span className="v">{trendfollowing?.samples ?? 0}</span></div>
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
        <div className="kv-row"><span className="k">direction</span><span className={`v ${histClass}`}>{dirLabel}</span></div>
        <div className="kv-row"><span className="k">last crossover</span><span className="v">{lastCrossLabel} · {timeAgo(trendfollowing?.lastCrossAt ?? null)}</span></div>
        <div className="kv-row"><span className="k">walk source</span><span className="v">GBM drift {walk.driftPerTick} · vol {(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>
    </div>
  );
}
