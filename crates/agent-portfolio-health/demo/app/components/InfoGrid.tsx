"use client";

import type { PortfolioHealthState } from "@/lib/portfoliohealth-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

function pct(n: number | null | undefined): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const s = n >= 0 ? "+" : "";
  return `${s}${n.toFixed(3)}%`;
}

function fmtAge(sec: number | null | undefined): string {
  if (sec === null || sec === undefined || Number.isNaN(sec)) return "—";
  const s = Math.max(0, Math.floor(sec));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  const rem = s % 60;
  return `${m}m${rem.toString().padStart(2, "0")}s`;
}

type Props = Readonly<{ portfoliohealth: PortfolioHealthState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ portfoliohealth, walk }: Props) {
  const c = portfoliohealth?.config;
  const stateLabel = portfoliohealth === null ? "idle" : portfoliohealth.status === "monitoring" ? "monitoring" : "stopped";

  const bidOrders = portfoliohealth?.openOrders.filter((o) => o.side === "BID") ?? [];
  const offerOrders = portfoliohealth?.openOrders.filter((o) => o.side === "OFFER") ?? [];
  const bidNotional = bidOrders.reduce((s, o) => s + o.notional, 0);
  const offerNotional = offerOrders.reduce((s, o) => s + o.notional, 0);
  const openOrdersCount = portfoliohealth?.openOrders.length ?? 0;

  const pending = portfoliohealth?.pendingSettlements ?? [];
  const pendingCount = pending.length;
  const pendingNotional = pending.reduce((s, p) => s + p.notional, 0);
  const oldestPendingAge = pending.reduce((m, p) => Math.max(m, p.ageSec), 0);

  const pv = portfoliohealth?.portfolioValueQuote ?? null;
  const startPv = portfoliohealth?.startPortfolioValueQuote ?? null;
  const pvChangePct =
    pv !== null && startPv !== null && startPv > 0 ? ((pv - startPv) / startPv) * 100 : null;
  const pvClass = pvChangePct === null ? "" : pvChangePct > 0 ? "positive" : pvChangePct < 0 ? "negative" : "";

  const balances = portfoliohealth?.balances ?? [];

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">portfolio-health</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PH CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">markets</span><span className="v mono">{c.markets.join(", ")}</span></div>
            <div className="kv-row"><span className="k">instruments</span><span className="v">{c.instruments.length}</span></div>
            <div className="kv-row"><span className="k">snapshot interval</span><span className="v accent">{c.snapshotIntervalSecs}s</span></div>
            <div className="kv-row"><span className="k">starting price</span><span className="v">{fmt(c.startingPrice)}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">markets</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">instruments</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">snapshot interval</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">starting price</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">BALANCES</div></div>
        {balances.length === 0 && (
          <div className="kv-row"><span className="k">—</span><span className="v faint">no data</span></div>
        )}
        {balances.map((b) => (
          <div key={b.instrument} className="kv-row">
            <span className="k">{b.instrument}</span>
            <span className="v">{fmt(b.balance)}</span>
          </div>
        ))}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">OPEN ORDERS</div></div>
        <div className="kv-row"><span className="k">orders</span><span className="v">{openOrdersCount}</span></div>
        <div className="kv-row"><span className="k">bid notional</span><span className="v positive">{fmt(bidNotional)}</span></div>
        <div className="kv-row"><span className="k">offer notional</span><span className="v negative">{fmt(offerNotional)}</span></div>
        <div className="kv-row"><span className="k">markets</span><span className="v">{new Set(portfoliohealth?.openOrders.map((o) => o.market)).size}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PENDING SETTLEMENTS</div></div>
        <div className="kv-row"><span className="k">count</span><span className="v">{pendingCount}</span></div>
        <div className="kv-row"><span className="k">total notional</span><span className="v">{fmt(pendingNotional)}</span></div>
        <div className="kv-row"><span className="k">oldest age</span><span className="v">{fmtAge(oldestPendingAge)}</span></div>
        <div className="kv-row"><span className="k">walk vol</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PORTFOLIO VALUE</div></div>
        <div className="kv-row"><span className="k">value (quote)</span><span className="v accent">{fmt(pv)}</span></div>
        <div className="kv-row"><span className="k">change since start</span><span className={`v ${pvClass}`}>{pct(pvChangePct)}</span></div>
        <div className="kv-row"><span className="k">snapshots</span><span className="v">{portfoliohealth?.snapshotsCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">current mid</span><span className="v">{fmt(portfoliohealth?.currentPrice)}</span></div>
      </div>
    </div>
  );
}
