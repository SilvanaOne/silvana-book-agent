"use client";

import type { ArbitrageState } from "@/lib/arbitrage-engine";
import type { WalkParams } from "@/lib/spread-simulator";

function usd(n: number | null | undefined): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  return `$${n.toFixed(2)}`;
}

type Props = Readonly<{ arbitrage: ArbitrageState | null; walk: WalkParams }>;

export function InfoGrid({ arbitrage, walk }: Props) {
  const c = arbitrage?.config;
  const focusBps = arbitrage ? arbitrage.pairBps[arbitrage.config.focusPair] : undefined;

  const now = Date.now();
  const nextScanIn =
    arbitrage && c
      ? Math.max(0, Math.ceil((arbitrage.lastScanAt + c.scanIntervalSecs * 1000 - now) / 1000))
      : null;
  const stateLabel = arbitrage === null ? "idle" : arbitrage.status === "scanning" ? "scanning" : "stopped";
  const activeVenues = arbitrage ? arbitrage.byVenue.filter((v) => v.count > 0).length : 0;

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">arbitrage</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">SCAN CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">focus pair</span><span className="v">{c.focusPair}</span></div>
            <div className="kv-row"><span className="k">act threshold</span><span className="v accent">≥ {c.minSpreadBps} bps</span></div>
            <div className="kv-row"><span className="k">trade size</span><span className="v">${c.tradeSizeUsd}</span></div>
            <div className="kv-row"><span className="k">scan interval</span><span className="v">{c.scanIntervalSecs}s</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">focus pair</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">act threshold</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">trade size</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">scan interval</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LIVE STATE</div></div>
        <div className="kv-row"><span className="k">focus spread</span><span className="v accent">{focusBps !== undefined ? `${Math.round(focusBps)} bps` : "—"}</span></div>
        <div className="kv-row"><span className="k">best seen</span><span className="v positive">{arbitrage ? `${arbitrage.bestBps} bps` : "—"}</span></div>
        <div className="kv-row"><span className="k">avg spread</span><span className="v">{arbitrage ? `${arbitrage.avgBps.toFixed(0)} bps` : "—"}</span></div>
        <div className="kv-row"><span className="k">next scan in</span><span className="v">{nextScanIn === null ? "—" : `${nextScanIn}s`}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">SCANNER</div></div>
        <div className="kv-row"><span className="k">scan cycles</span><span className="v">{arbitrage?.scans ?? 0}</span></div>
        <div className="kv-row"><span className="k">spreads found</span><span className="v">{arbitrage?.spreadsFound ?? 0}</span></div>
        <div className="kv-row"><span className="k">acted</span><span className="v accent">{arbitrage?.actedCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">active venues</span><span className="v">{activeVenues}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PROFITABILITY</div></div>
        <div className="kv-row"><span className="k">est. profit Σ</span><span className="v accent">{usd(arbitrage?.estProfitUsd)}</span></div>
        <div className="kv-row"><span className="k">realized (acted)</span><span className="v positive">{usd(arbitrage?.realizedUsd)}</span></div>
        <div className="kv-row"><span className="k">avg per opp</span><span className="v">{arbitrage && arbitrage.spreadsFound > 0 ? usd(arbitrage.estProfitUsd / arbitrage.spreadsFound) : "—"}</span></div>
        <div className="kv-row"><span className="k">hit rate</span><span className="v">{arbitrage && arbitrage.spreadsFound > 0 ? `${((arbitrage.actedCount / arbitrage.spreadsFound) * 100).toFixed(0)}%` : "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">SPREAD SIM</div></div>
        <div className="kv-row"><span className="k">source</span><span className="v">mean-revert walk</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick} bps</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{walk.volPerTick} bps</span></div>
        <div className="kv-row"><span className="k">tick spacing</span><span className="v">1000 ms</span></div>
      </div>
    </div>
  );
}
