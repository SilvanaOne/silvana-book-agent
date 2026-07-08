"use client";

import type { TradingHistoryState } from "@/lib/tradinghistory-engine";

type Props = Readonly<{ tradinghistory: TradingHistoryState | null; walk: { driftPerTick: number; volPerTick: number } }>;

function tsShort(t?: number): string {
  if (!t) return "—";
  const d = new Date(t);
  const pad = (n: number) => n.toString().padStart(2, "0");
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

function ratePerMin(win: number[] | undefined): string {
  if (!win || win.length === 0) return "0";
  return win.length.toString();
}

export function InfoGrid({ tradinghistory, walk }: Props) {
  const c = tradinghistory?.config;
  const stateLabel = tradinghistory === null ? "idle" : tradinghistory.status === "monitoring" ? "recording" : "stopped";
  const streams = c ? [c.orders ? "orders" : null, c.settlements ? "settlements" : null].filter(Boolean).join("+") : "—";
  const chainClass = tradinghistory === null ? "" : tradinghistory.chainOK ? "positive" : "negative";
  const chainVal = tradinghistory === null ? "—" : tradinghistory.chainOK ? "Y" : "N";
  const eventArrival = c?.eventArrivalPerTick ?? 0;
  const expectedNextSec = eventArrival > 0 ? (1 / eventArrival).toFixed(2) : "∞";

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">trading-history</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">JSONL (sim)</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TH CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">subscriptions</span><span className="v accent">{streams}</span></div>
            <div className="kv-row"><span className="k">market filter</span><span className="v">{c.market ?? "* (all)"}</span></div>
            <div className="kv-row"><span className="k">arrival λ</span><span className="v">{c.eventArrivalPerTick}/tick</span></div>
            <div className="kv-row"><span className="k">starting price</span><span className="v">{c.startingPrice}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">subscriptions</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">market filter</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">arrival λ</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">starting price</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RECORDS</div></div>
        <div className="kv-row"><span className="k">total (lifetime)</span><span className="v">{tradinghistory?.recordsCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">rate / min</span><span className="v">{ratePerMin(tradinghistory?.rateWindow)}</span></div>
        <div className="kv-row"><span className="k">last kind</span><span className="v accent">{tradinghistory?.lastKind ?? "—"}</span></div>
        <div className="kv-row"><span className="k">last hash</span><span className="v mono">{tradinghistory?.lastHash ? tradinghistory.lastHash.slice(0, 12) : "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">CHAIN</div></div>
        <div className="kv-row"><span className="k">chain OK</span><span className={`v ${chainClass}`}>{chainVal}</span></div>
        <div className="kv-row"><span className="k">tampered @ idx</span><span className="v">{tradinghistory?.tamperedIndex ?? "—"}</span></div>
        <div className="kv-row"><span className="k">verified count</span><span className="v accent">{tradinghistory?.verifiedCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">records shown</span><span className="v">{tradinghistory?.records.length ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">INTEGRITY</div></div>
        <div className="kv-row"><span className="k">last verified</span><span className="v">{tsShort(tradinghistory?.lastVerifiedAt)}</span></div>
        <div className="kv-row"><span className="k">next event in</span><span className="v">~{expectedNextSec}s</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
        <div className="kv-row"><span className="k">last seq</span><span className="v">{tradinghistory?.lastSeq ?? "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATS</div></div>
        <div className="kv-row"><span className="k">current mid</span><span className="v">{tradinghistory ? tradinghistory.currentPrice.toFixed(6) : "—"}</span></div>
        <div className="kv-row"><span className="k">source</span><span className="v">GBM walk</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>
    </div>
  );
}
