"use client";

import type { TrendAnalysisState } from "@/lib/trendanalysis-engine";

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

function bps(n: number | null | undefined): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const s = n >= 0 ? "+" : "";
  return `${s}${n.toFixed(1)} bps`;
}

function rsiState(rsi: number | null): { label: string; klass: string } {
  if (rsi === null) return { label: "—", klass: "faint" };
  if (rsi < 30) return { label: "oversold", klass: "positive" };
  if (rsi > 70) return { label: "overbought", klass: "negative" };
  return { label: "neutral", klass: "" };
}

function macdState(hist: number | null): { label: string; klass: string } {
  if (hist === null) return { label: "—", klass: "faint" };
  if (hist > 0) return { label: "bullish", klass: "positive" };
  if (hist < 0) return { label: "bearish", klass: "negative" };
  return { label: "flat", klass: "" };
}

type Props = Readonly<{ trendanalysis: TrendAnalysisState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ trendanalysis, walk }: Props) {
  const c = trendanalysis?.config;
  const stateLabel = trendanalysis === null ? "idle" : trendanalysis.status === "monitoring" ? "publishing" : "stopped";

  // Mid vs SMA spread in bps.
  let midVsSmaBps: number | null = null;
  if (trendanalysis && trendanalysis.sma !== null && trendanalysis.sma > 0) {
    midVsSmaBps = ((trendanalysis.currentPrice - trendanalysis.sma) / trendanalysis.sma) * 10000;
  }

  const rsi = trendanalysis?.rsi ?? null;
  const rState = rsiState(rsi);
  const mState = macdState(trendanalysis?.macdHistogram ?? null);

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">trend-analysis</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TA CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v">{c.market}</span></div>
            <div className="kv-row"><span className="k">window</span><span className="v">{c.window}</span></div>
            <div className="kv-row"><span className="k">rsi period</span><span className="v">{c.rsiPeriod}</span></div>
            <div className="kv-row"><span className="k">bollinger k</span><span className="v accent">±{c.bollingerK}σ</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">window</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">rsi period</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">bollinger k</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">MOVING AVGS</div></div>
        <div className="kv-row"><span className="k">mid</span><span className="v">{fmt(trendanalysis?.currentPrice)}</span></div>
        <div className="kv-row"><span className="k">SMA</span><span className="v">{fmt(trendanalysis?.sma ?? null)}</span></div>
        <div className="kv-row"><span className="k">EMA</span><span className="v accent">{fmt(trendanalysis?.ema ?? null)}</span></div>
        <div className="kv-row"><span className="k">mid vs SMA</span><span className="v">{bps(midVsSmaBps)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">BOLLINGER</div></div>
        <div className="kv-row"><span className="k">upper</span><span className="v">{fmt(trendanalysis?.bollingerUpper ?? null)}</span></div>
        <div className="kv-row"><span className="k">middle (SMA)</span><span className="v">{fmt(trendanalysis?.bollingerMiddle ?? null)}</span></div>
        <div className="kv-row"><span className="k">lower</span><span className="v">{fmt(trendanalysis?.bollingerLower ?? null)}</span></div>
        <div className="kv-row"><span className="k">std / pos</span><span className="v">{fmt(trendanalysis?.bollingerStd ?? null)}{trendanalysis?.bandPositionPct !== null && trendanalysis?.bandPositionPct !== undefined ? ` / ${trendanalysis.bandPositionPct.toFixed(1)}%` : ""}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RSI</div></div>
        <div className="kv-row"><span className="k">value</span><span className={`v ${rState.klass}`}>{rsi === null ? "—" : rsi.toFixed(2)}</span></div>
        <div className="kv-row"><span className="k">state</span><span className={`v ${rState.klass}`}>{rState.label}</span></div>
        <div className="kv-row"><span className="k">avg gain</span><span className="v">{fmt(trendanalysis?.rsiAvgGain ?? null)}</span></div>
        <div className="kv-row"><span className="k">avg loss</span><span className="v">{fmt(trendanalysis?.rsiAvgLoss ?? null)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">MACD</div></div>
        <div className="kv-row"><span className="k">line ({c ? c.macdFast : "—"}/{c ? c.macdSlow : "—"})</span><span className="v">{fmt(trendanalysis?.macdLine ?? null)}</span></div>
        <div className="kv-row"><span className="k">signal ({c ? c.macdSignal : "—"})</span><span className="v accent">{fmt(trendanalysis?.macdSignalLine ?? null)}</span></div>
        <div className="kv-row"><span className="k">histogram</span><span className={`v ${mState.klass}`}>{fmt(trendanalysis?.macdHistogram ?? null)}</span></div>
        <div className="kv-row"><span className="k">state</span><span className={`v ${mState.klass}`}>{mState.label}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PUBLISH</div></div>
        <div className="kv-row"><span className="k">records</span><span className="v accent">{trendanalysis?.publishedCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">last at</span><span className="v">{trendanalysis?.lastPublishedAt ? new Date(trendanalysis.lastPublishedAt).toLocaleTimeString() : "—"}</span></div>
        <div className="kv-row"><span className="k">source</span><span className="v">GBM walk (sim)</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>
    </div>
  );
}
