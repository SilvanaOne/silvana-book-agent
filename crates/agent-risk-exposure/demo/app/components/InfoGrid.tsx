"use client";

import type { RiskExposureState } from "@/lib/riskexposure-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

function fmtQuote(n: number | null | undefined): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  if (abs >= 1000) return `$${n.toFixed(0)}`;
  if (abs >= 1) return `$${n.toFixed(2)}`;
  return `$${n.toFixed(4)}`;
}

function pct(n: number | null | undefined): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const s = n >= 0 ? "+" : "";
  return `${s}${n.toFixed(3)}%`;
}

type Props = Readonly<{ riskexposure: RiskExposureState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ riskexposure, walk }: Props) {
  const c = riskexposure?.config;
  const stateLabel = riskexposure === null ? "idle" : riskexposure.status === "monitoring" ? "monitoring" : "stopped";

  const topPos = riskexposure && riskexposure.positions.length > 0
    ? riskexposure.positions.reduce((a, b) => (a.sharePct > b.sharePct ? a : b))
    : null;

  const totalOpenNotional = riskexposure?.openNotionalPerMarket.reduce((s, o) => s + o.notional, 0) ?? 0;
  const maxMarket = riskexposure && riskexposure.openNotionalPerMarket.length > 0
    ? riskexposure.openNotionalPerMarket.reduce((a, b) => (a.notional > b.notional ? a : b))
    : null;

  const changeFromStart = riskexposure && riskexposure.startingPortfolioQuote > 0
    ? ((riskexposure.totalPortfolioQuote - riskexposure.startingPortfolioQuote) / riskexposure.startingPortfolioQuote) * 100
    : null;
  const changeClass = changeFromStart === null ? "" : changeFromStart > 0 ? "positive" : changeFromStart < 0 ? "negative" : "";
  const warnClass = riskexposure?.concentrationWarn ? "negative" : "";

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">risk-exposure</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RE CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">markets</span><span className="v">{c.markets.length} · {c.markets.join(",")}</span></div>
            <div className="kv-row"><span className="k">instruments</span><span className="v">{c.instruments.length}</span></div>
            <div className="kv-row"><span className="k">warn threshold</span><span className="v accent">{c.concentrationWarnPct}%</span></div>
            <div className="kv-row"><span className="k">snapshot interval</span><span className="v">{c.snapshotIntervalSecs}s</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">markets</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">instruments</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">warn threshold</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">snapshot interval</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PORTFOLIO</div></div>
        <div className="kv-row"><span className="k">total value</span><span className="v accent">{fmtQuote(riskexposure?.totalPortfolioQuote)}</span></div>
        <div className="kv-row"><span className="k">change from start</span><span className={`v ${changeClass}`}>{pct(changeFromStart)}</span></div>
        <div className="kv-row"><span className="k">instruments</span><span className="v">{riskexposure?.positions.length ?? 0}</span></div>
        <div className="kv-row"><span className="k">current mid</span><span className="v">{fmt(riskexposure?.currentPrice)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">CONCENTRATION</div></div>
        <div className="kv-row"><span className="k">top instrument</span><span className="v">{topPos?.instrument ?? "—"}</span></div>
        <div className="kv-row"><span className="k">top share</span><span className={`v ${warnClass || "accent"}`}>{topPos ? `${topPos.sharePct.toFixed(2)}%` : "—"}</span></div>
        <div className="kv-row"><span className="k">warn</span><span className={`v ${warnClass}`}>{riskexposure?.concentrationWarn ? "Y" : "N"}</span></div>
        <div className="kv-row"><span className="k">warned inst</span><span className="v">{riskexposure?.concentrationWarn ? (riskexposure.warnedInstrument ?? "—") : "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">OPEN NOTIONAL</div></div>
        <div className="kv-row"><span className="k">total open</span><span className="v">{fmtQuote(totalOpenNotional)}</span></div>
        <div className="kv-row"><span className="k">max market</span><span className="v">{maxMarket?.market ?? "—"}</span></div>
        <div className="kv-row"><span className="k">max open</span><span className="v">{fmtQuote(maxMarket?.notional)}</span></div>
        <div className="kv-row"><span className="k">pending settle</span><span className="v">{fmtQuote(riskexposure?.pendingSettlementsNotional)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATS</div></div>
        <div className="kv-row"><span className="k">snapshots</span><span className="v accent">{riskexposure?.snapshotsCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">price source</span><span className="v">GBM walk</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>
    </div>
  );
}
