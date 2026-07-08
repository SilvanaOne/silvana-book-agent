"use client";

import type { HedgingState } from "@/lib/hedging-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

function fmtQty(n: number | null | undefined): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 3 : 4;
  return n.toFixed(digits);
}

function sgn(n: number): string {
  return n >= 0 ? "+" : "";
}

type Props = Readonly<{ hedging: HedgingState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ hedging, walk }: Props) {
  const c = hedging?.config;
  const delta = hedging && c ? hedging.currentBalance - c.targetBalance : null;
  const inBand = delta !== null && c ? Math.abs(delta) <= c.tolerance : false;
  const openOrders = hedging?.orders.filter((o) => o.status === "open").length ?? 0;
  const stateLabel = hedging === null ? "idle" : hedging.status === "monitoring" ? (inBand ? "in-band" : "hedging") : "stopped";
  const now = Date.now();
  const nextCheckIn = hedging && hedging.status === "monitoring" ? Math.max(0, Math.ceil((hedging.nextCheckAt - now) / 1000)) : null;

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">hedging</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">HEDGE CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">instrument</span><span className="v">{c.exposureInstrument}</span></div>
            <div className="kv-row"><span className="k">hedge market</span><span className="v">{c.hedgeMarket}</span></div>
            <div className="kv-row"><span className="k">target ± tol</span><span className="v accent">{fmtQty(c.targetBalance)} ± {fmtQty(c.tolerance)}</span></div>
            <div className="kv-row"><span className="k">check interval</span><span className="v">{c.checkIntervalSecs}s</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">instrument</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">hedge market</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">target ± tol</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">check interval</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LIVE STATE</div></div>
        <div className="kv-row"><span className="k">balance</span><span className="v">{fmtQty(hedging?.currentBalance)}</span></div>
        <div className="kv-row"><span className="k">deviation</span><span className={`v ${delta !== null && !inBand ? "accent" : ""}`}>{delta === null ? "—" : `${sgn(delta)}${fmtQty(Math.abs(delta))}`}</span></div>
        <div className="kv-row"><span className="k">in-band</span><span className={`v ${inBand ? "positive" : "negative"}`}>{hedging === null ? "—" : inBand ? "Y" : "N"}</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">HEDGE</div></div>
        <div className="kv-row"><span className="k">hedges placed</span><span className="v accent">{hedging?.hedgesCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">last hedge side</span><span className={`v ${hedging?.lastHedgeSide === "BID" ? "positive" : hedging?.lastHedgeSide === "OFFER" ? "negative" : ""}`}>{hedging?.lastHedgeSide ?? "—"}</span></div>
        <div className="kv-row"><span className="k">hedge fraction</span><span className="v">{c ? c.hedgeFraction : "—"}</span></div>
        <div className="kv-row"><span className="k">next check</span><span className="v">{nextCheckIn === null ? "—" : `${nextCheckIn}s`}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">FILLS</div></div>
        <div className="kv-row"><span className="k">open orders</span><span className="v">{openOrders}</span></div>
        <div className="kv-row"><span className="k">orders placed</span><span className="v">{hedging?.ordersPlaced ?? 0}</span></div>
        <div className="kv-row"><span className="k">orders filled</span><span className="v accent">{hedging?.ordersFilled ?? 0}</span></div>
        <div className="kv-row"><span className="k">unfilled</span><span className="v">{hedging ? Math.max(0, hedging.ordersPlaced - hedging.ordersFilled) : 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PRICE SIM</div></div>
        <div className="kv-row"><span className="k">current mid</span><span className="v">{fmt(hedging?.currentPrice)}</span></div>
        <div className="kv-row"><span className="k">exposure drift σ</span><span className="v">{c ? fmtQty(c.exposureDriftPerTick) : "—"}</span></div>
        <div className="kv-row"><span className="k">price drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">price vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>
    </div>
  );
}
