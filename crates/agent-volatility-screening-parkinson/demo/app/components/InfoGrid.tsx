"use client";

import type { VolatilityScreeningState } from "@/lib/volatilityscreening-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n) || !Number.isFinite(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

function pctPos(n: number | null | undefined, digits = 2): string {
  if (n === null || n === undefined || Number.isNaN(n) || !Number.isFinite(n)) return "—";
  return `${(n * 100).toFixed(digits)}%`;
}

function sci(n: number | null | undefined): string {
  if (n === null || n === undefined || Number.isNaN(n) || !Number.isFinite(n)) return "—";
  return n.toExponential(3);
}

type Props = Readonly<{ volatilityscreening: VolatilityScreeningState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ volatilityscreening, walk }: Props) {
  const c = volatilityscreening?.config;
  const s = volatilityscreening;
  const bar = s?.currentBar ?? null;

  const stateLabel = s === null
    ? "idle"
    : s.status !== "monitoring"
    ? "stopped"
    : s.publishedCount === 0
    ? "warmup"
    : "publishing";

  const windowFillPct = s && c ? Math.min(100, (s.closedBars.length / c.window) * 100) : 0;

  const currentRange = bar && bar.low > 0 ? bar.high / bar.low - 1 : null;

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">parkinson</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">tick interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">VOL CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v">{c.market}</span></div>
            <div className="kv-row"><span className="k">window</span><span className="v">{c.window} bars</span></div>
            <div className="kv-row"><span className="k">period</span><span className="v">{c.periodSecs}s</span></div>
            <div className="kv-row"><span className="k">periods / year</span><span className="v accent">{c.periodsPerYear.toLocaleString()}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">window</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">period</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">periods / year</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">CURRENT BAR</div></div>
        <div className="kv-row"><span className="k">mid</span><span className="v">{fmt(s?.currentPrice)}</span></div>
        <div className="kv-row"><span className="k">current H</span><span className="v">{fmt(bar?.high ?? null)}</span></div>
        <div className="kv-row"><span className="k">current L</span><span className="v">{fmt(bar?.low ?? null)}</span></div>
        <div className="kv-row"><span className="k">bar range</span><span className="v">{pctPos(currentRange, 3)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">BUFFER</div></div>
        <div className="kv-row"><span className="k">closed bars</span><span className="v">{s?.closedBars.length ?? 0}{c ? ` / ${c.window}` : ""}</span></div>
        <div className="kv-row"><span className="k">samples in bar</span><span className="v">{bar?.samples ?? 0}</span></div>
        <div className="kv-row"><span className="k">window filled</span><span className="v">{s?.windowFull ? "Y" : "N"} ({windowFillPct.toFixed(0)}%)</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PARKINSON</div></div>
        <div className="kv-row"><span className="k">sigma / period</span><span className="v">{sci(s?.sigmaPerPeriod ?? null)}</span></div>
        <div className="kv-row"><span className="k">sigma annualized</span><span className="v accent">{pctPos(s?.sigmaAnnualized ?? null)}</span></div>
        <div className="kv-row"><span className="k">agg high</span><span className="v">{fmt(s?.aggHigh ?? null)}</span></div>
        <div className="kv-row"><span className="k">agg low</span><span className="v">{fmt(s?.aggLow ?? null)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PRICE SIM</div></div>
        <div className="kv-row"><span className="k">source</span><span className="v">GBM walk</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
        <div className="kv-row"><span className="k">override</span><span className="v">manual jump ok</span></div>
      </div>
    </div>
  );
}
