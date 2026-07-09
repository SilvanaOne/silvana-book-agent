"use client";

import type { TrendAnalysisState, Alignment } from "@/lib/trendanalysis-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

function fmtSlope(n: number | null | undefined): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 1 ? 4 : 8;
  const sign = n >= 0 ? "+" : "";
  return `${sign}${n.toFixed(digits)}`;
}

function slopeKlass(n: number | null | undefined): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "faint";
  if (n > 0) return "positive";
  if (n < 0) return "negative";
  return "";
}

function alignmentBadge(a: Alignment | undefined): { label: string; klass: string } {
  switch (a) {
    case "aligned_up":   return { label: "aligned ↑", klass: "positive" };
    case "aligned_down": return { label: "aligned ↓", klass: "negative" };
    case "mixed":        return { label: "mixed",     klass: "" };
    case "warmup":
    default:             return { label: "warmup",    klass: "faint" };
  }
}

type Props = Readonly<{ trendanalysis: TrendAnalysisState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ trendanalysis, walk }: Props) {
  const c = trendanalysis?.config;
  const stateLabel = trendanalysis === null ? "idle" : trendanalysis.status === "monitoring" ? "publishing" : "stopped";
  const align = alignmentBadge(trendanalysis?.alignment);
  const samples = trendanalysis?.priceHistory.length ?? 0;

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">multi-timeframe</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">MTF CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v">{c.market}</span></div>
            <div className="kv-row"><span className="k">short</span><span className="v accent">{c.short}</span></div>
            <div className="kv-row"><span className="k">mid</span><span className="v accent">{c.mid}</span></div>
            <div className="kv-row"><span className="k">long</span><span className="v accent">{c.long}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">short</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">mid</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">long</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">MID · SMAs</div></div>
        <div className="kv-row"><span className="k">mid</span><span className="v">{fmt(trendanalysis?.currentPrice)}</span></div>
        <div className="kv-row"><span className="k">SMA short ({c ? c.short : "—"})</span><span className="v">{fmt(trendanalysis?.smaShort ?? null)}</span></div>
        <div className="kv-row"><span className="k">SMA mid ({c ? c.mid : "—"})</span><span className="v">{fmt(trendanalysis?.smaMid ?? null)}</span></div>
        <div className="kv-row"><span className="k">SMA long ({c ? c.long : "—"})</span><span className="v">{fmt(trendanalysis?.smaLong ?? null)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">SLOPES</div></div>
        <div className="kv-row"><span className="k">slope short</span><span className={`v ${slopeKlass(trendanalysis?.slopeShort ?? null)}`}>{fmtSlope(trendanalysis?.slopeShort ?? null)}</span></div>
        <div className="kv-row"><span className="k">slope mid</span><span className={`v ${slopeKlass(trendanalysis?.slopeMid ?? null)}`}>{fmtSlope(trendanalysis?.slopeMid ?? null)}</span></div>
        <div className="kv-row"><span className="k">slope long</span><span className={`v ${slopeKlass(trendanalysis?.slopeLong ?? null)}`}>{fmtSlope(trendanalysis?.slopeLong ?? null)}</span></div>
        <div className="kv-row"><span className="k">alignment</span><span className={`v ${align.klass}`}>{align.label}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">BUFFER</div></div>
        <div className="kv-row"><span className="k">samples</span><span className="v">{samples}{c ? ` / ${c.long}` : ""}</span></div>
        <div className="kv-row"><span className="k">warmup</span><span className="v">{c && samples < c.long ? `${c.long - samples} more` : "complete"}</span></div>
        <div className="kv-row"><span className="k">records</span><span className="v accent">{trendanalysis?.publishedCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">last at</span><span className="v">{trendanalysis?.lastPublishedAt ? new Date(trendanalysis.lastPublishedAt).toLocaleTimeString() : "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">SOURCE</div></div>
        <div className="kv-row"><span className="k">source</span><span className="v">GBM walk (sim)</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{(walk.driftPerTick * 100).toFixed(3)}%</span></div>
      </div>
    </div>
  );
}
