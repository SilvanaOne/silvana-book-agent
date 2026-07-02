"use client";

import type { DcaPortfolioState } from "@/lib/dcaportfolio-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

type Props = Readonly<{
  dcaportfolio: DcaPortfolioState | null;
  walk: { driftPerTick: number; volPerTick: number };
}>;

export function InfoGrid({ dcaportfolio, walk }: Props) {
  const c = dcaportfolio?.config;

  // Aggregate totals across markets
  const totals = (dcaportfolio?.progress ?? []).reduce(
    (acc, mp) => {
      acc.orders += mp.orderCount;
      acc.qty += mp.totalQty;
      acc.notional += mp.totalNotional;
      return acc;
    },
    { orders: 0, qty: 0, notional: 0 },
  );

  const stateLabel =
    dcaportfolio === null
      ? "idle"
      : dcaportfolio.status === "running"
        ? "running"
        : dcaportfolio.status === "completed"
          ? "completed"
          : "stopped";

  // Cadence: how many seconds until the next order across any market
  let nextInSec: number | null = null;
  if (dcaportfolio && dcaportfolio.status === "running" && c) {
    const now = Date.now();
    const intervalMs = c.intervalSecs * 1000;
    for (const mp of dcaportfolio.progress) {
      if (mp.completed) continue;
      const lastAt = mp.lastOrderAt ?? dcaportfolio.startedAt;
      const remaining = Math.max(0, (lastAt + intervalMs - now) / 1000);
      if (nextInSec === null || remaining < nextInSec) nextInSec = remaining;
    }
  }

  const uptimeSec = dcaportfolio ? Math.max(0, (Date.now() - dcaportfolio.startedAt) / 1000) : 0;

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">dca-portfolio</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">DP CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">markets</span><span className="v">{c.markets.length}</span></div>
            <div className="kv-row"><span className="k">side</span><span className="v accent">{c.side.toUpperCase()}</span></div>
            <div className="kv-row"><span className="k">amount / order</span><span className="v">{c.amountPerOrder}</span></div>
            <div className="kv-row"><span className="k">interval</span><span className="v">{c.intervalSecs}s</span></div>
            <div className="kv-row"><span className="k">price offset</span><span className="v">{c.priceOffsetPct}%</span></div>
            <div className="kv-row"><span className="k">max total / market</span><span className="v">{c.maxTotal === null ? "∞" : c.maxTotal}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">markets</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">side</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">amount / order</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">interval</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PROGRESS</div></div>
        {dcaportfolio && dcaportfolio.progress.length > 0 ? (
          dcaportfolio.progress.map((mp) => (
            <div key={mp.market} className="kv-row">
              <span className="k mono">{mp.market}</span>
              <span className="v mono">
                {mp.orderCount} ord · {fmt(mp.totalQty)} qty · avg {mp.avgPrice > 0 ? fmt(mp.avgPrice) : "—"}
                {mp.completed ? " · done" : ""}
              </span>
            </div>
          ))
        ) : (
          <div className="kv-row"><span className="k">markets</span><span className="v faint">—</span></div>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TOTALS</div></div>
        <div className="kv-row"><span className="k">total orders</span><span className="v accent">{totals.orders}</span></div>
        <div className="kv-row"><span className="k">total qty</span><span className="v">{fmt(totals.qty)}</span></div>
        <div className="kv-row"><span className="k">total notional</span><span className="v">{fmt(totals.notional)}</span></div>
        <div className="kv-row"><span className="k">completed markets</span><span className="v">{dcaportfolio?.completedMarkets ?? 0} / {c?.markets.length ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">CADENCE</div></div>
        <div className="kv-row"><span className="k">next order in</span><span className="v accent">{nextInSec === null ? "—" : `${nextInSec.toFixed(1)}s`}</span></div>
        <div className="kv-row"><span className="k">uptime</span><span className="v">{uptimeSec.toFixed(0)}s</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATE</div></div>
        <div className="kv-row"><span className="k">status</span><span className="v accent">{stateLabel}</span></div>
        <div className="kv-row"><span className="k">markets running</span><span className="v">{(c?.markets.length ?? 0) - (dcaportfolio?.completedMarkets ?? 0)}</span></div>
        <div className="kv-row"><span className="k">source</span><span className="v">GBM walk (per market)</span></div>
      </div>
    </div>
  );
}
