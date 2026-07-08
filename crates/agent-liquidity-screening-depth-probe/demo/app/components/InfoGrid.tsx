"use client";

import type { LiquidityScreeningState } from "@/lib/liquidityscreening-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

function fmtBps(n: number | null | undefined): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  return `${n.toFixed(2)} bps`;
}

function fmtPct(n: number | null | undefined): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const s = n >= 0 ? "+" : "";
  return `${s}${n.toFixed(2)}%`;
}

type Props = Readonly<{ liquidityscreening: LiquidityScreeningState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ liquidityscreening, walk }: Props) {
  const c = liquidityscreening?.config;
  const stateLabel = liquidityscreening === null ? "idle" : liquidityscreening.status === "monitoring" ? "streaming" : "stopped";
  const bestBid = liquidityscreening?.bidLevels[0]?.price;
  const bestOffer = liquidityscreening?.offerLevels[0]?.price;
  const bidTot = liquidityscreening?.bidDepthTotal ?? 0;
  const offerTot = liquidityscreening?.offerDepthTotal ?? 0;
  const imbalance = bidTot + offerTot > 0 ? ((bidTot - offerTot) / (bidTot + offerTot)) * 100 : null;

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">liquidity-screening</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">tick interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LIQ CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v">{c.market}</span></div>
            <div className="kv-row"><span className="k">probe qty</span><span className="v accent">{c.probeQty}</span></div>
            <div className="kv-row"><span className="k">depth / side</span><span className="v">{c.depth}</span></div>
            <div className="kv-row"><span className="k">poll secs</span><span className="v">{c.pollSecs}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">probe qty</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">depth / side</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">poll secs</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">SPREAD</div></div>
        <div className="kv-row"><span className="k">best bid</span><span className="v positive">{fmt(bestBid)}</span></div>
        <div className="kv-row"><span className="k">best offer</span><span className="v negative">{fmt(bestOffer)}</span></div>
        <div className="kv-row"><span className="k">spread</span><span className="v">{fmt(liquidityscreening?.spread)}</span></div>
        <div className="kv-row"><span className="k">spread bps</span><span className="v accent">{fmtBps(liquidityscreening?.spreadBps)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">DEPTH TOTALS</div></div>
        <div className="kv-row"><span className="k">bid depth</span><span className="v positive">{fmt(bidTot, 2)}</span></div>
        <div className="kv-row"><span className="k">offer depth</span><span className="v negative">{fmt(offerTot, 2)}</span></div>
        <div className="kv-row"><span className="k">bid notional</span><span className="v">{fmt(liquidityscreening?.bidNotionalTotal, 2)}</span></div>
        <div className="kv-row"><span className="k">imbalance</span><span className={`v ${imbalance !== null && Math.abs(imbalance) > 20 ? "accent" : ""}`}>{fmtPct(imbalance)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">SLIPPAGE</div></div>
        <div className="kv-row"><span className="k">buy vwap</span><span className="v negative">{fmt(liquidityscreening?.buyProbeVwap ?? null)}</span></div>
        <div className="kv-row"><span className="k">buy slip</span><span className="v accent">{fmtBps(liquidityscreening?.buyProbeSlippageBps ?? null)}</span></div>
        <div className="kv-row"><span className="k">sell vwap</span><span className="v positive">{fmt(liquidityscreening?.sellProbeVwap ?? null)}</span></div>
        <div className="kv-row"><span className="k">sell slip</span><span className="v accent">{fmtBps(liquidityscreening?.sellProbeSlippageBps ?? null)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATS</div></div>
        <div className="kv-row"><span className="k">snapshots</span><span className="v">{liquidityscreening?.snapshotsCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>
    </div>
  );
}
