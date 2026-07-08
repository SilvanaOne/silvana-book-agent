"use client";

import type { VolatilityScreeningState } from "@/lib/volatilityscreening-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

function pct(n: number | null | undefined, digits = 3): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const s = n >= 0 ? "+" : "";
  return `${s}${(n * 100).toFixed(digits)}%`;
}

function pctPos(n: number | null | undefined, digits = 2): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  return `${(n * 100).toFixed(digits)}%`;
}

function sci(n: number | null | undefined): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  return n.toExponential(3);
}

type Props = Readonly<{ volatilityscreening: VolatilityScreeningState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ volatilityscreening, walk }: Props) {
  const c = volatilityscreening?.config;
  const s = volatilityscreening;
  const lastLogReturn = s && s.currentPrice > 0 && s.prevPrice !== null && s.prevPrice > 0
    ? Math.log(s.currentPrice / s.prevPrice)
    : null;

  const stateLabel = s === null
    ? "idle"
    : s.status !== "monitoring"
    ? "stopped"
    : s.publishedCount === 0
    ? "warmup"
    : "publishing";

  const windowFillPct = s && c ? Math.min(100, (s.returnsWindow.length / c.window) * 100) : 0;

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">volatility-screening</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">tick interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">VOL CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v">{c.market}</span></div>
            <div className="kv-row"><span className="k">window</span><span className="v">{c.window} samples</span></div>
            <div className="kv-row"><span className="k">poll interval</span><span className="v">{c.pollSecs}s</span></div>
            <div className="kv-row"><span className="k">periods / year</span><span className="v accent">{c.periodsPerYear.toLocaleString()}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">window</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">poll interval</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">periods / year</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LIVE STATE</div></div>
        <div className="kv-row"><span className="k">mid</span><span className="v">{fmt(s?.currentPrice)}</span></div>
        <div className="kv-row"><span className="k">prev mid</span><span className="v">{fmt(s?.prevPrice ?? null)}</span></div>
        <div className="kv-row"><span className="k">last log return</span><span className="v">{pct(lastLogReturn)}</span></div>
        <div className="kv-row"><span className="k">samples in window</span><span className="v">{s?.returnsWindow.length ?? 0}{c ? ` / ${c.window}` : ""}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">STATISTICS</div></div>
        <div className="kv-row"><span className="k">mean return</span><span className="v">{pct(s?.meanReturn ?? null, 4)}</span></div>
        <div className="kv-row"><span className="k">std of returns</span><span className="v">{sci(s?.stdReturn ?? null)}</span></div>
        <div className="kv-row"><span className="k">window filled</span><span className="v">{s?.windowFull ? "Y" : "N"} ({windowFillPct.toFixed(0)}%)</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">VOLATILITY</div></div>
        <div className="kv-row"><span className="k">realized vol (ann.)</span><span className="v accent">{pctPos(s?.realizedVolAnnualized ?? null)}</span></div>
        <div className="kv-row"><span className="k">log vol</span><span className="v">{fmt(s?.logVol ?? null)}</span></div>
        <div className="kv-row"><span className="k">published count</span><span className="v">{s?.publishedCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">ticks observed</span><span className="v">{s?.ticksObserved ?? 0}</span></div>
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
