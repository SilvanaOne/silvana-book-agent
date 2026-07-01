"use client";

import type { PositionState } from "@/lib/tpsl-engine";

function fmt(n: number | null, min = 4): string {
  if (n === null || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

type Props = Readonly<{ position: PositionState | null }>;

export function MetricStrip({ position }: Props) {
  const price = position?.currentPrice ?? null;
  const entry = position?.config.entryPrice ?? null;
  const pnl = position?.pnl ?? null;
  const pnlPct =
    position && entry && price !== null
      ? ((price - entry) / entry) * 100 * (position.config.side === "long" ? 1 : -1)
      : null;
  const pnlCls = pnl === null ? "" : pnl > 0 ? "positive" : pnl < 0 ? "negative" : "muted";

  return (
    <div className="metric-strip">
      <div className="metric-cell">
        <div className="metric-label">Current price</div>
        <div className="mono metric-value">{fmt(price)}</div>
        <div className="metric-sub mono">entry {fmt(entry)}</div>
      </div>
      <div className="metric-cell">
        <div className="metric-label">Unrealized PnL</div>
        <div className={`mono metric-value ${pnlCls}`}>
          {pnl === null ? "—" : `${pnl > 0 ? "+" : ""}${fmt(pnl, 6)}`}
        </div>
        <div className="metric-sub mono">
          {pnlPct === null ? "—" : `${pnlPct >= 0 ? "+" : ""}${pnlPct.toFixed(2)}%`}
        </div>
      </div>
      <div className="metric-cell">
        <div className="metric-label">Take Profit</div>
        <div className="mono metric-value">{fmt(position?.config.tp ?? null)}</div>
        <div className="metric-sub mono">{position?.config.tp !== null && position?.config.tp !== undefined ? "target" : "not set"}</div>
      </div>
      <div className="metric-cell">
        <div className="metric-label">Stop Loss{position?.config.trailingPct ? ` · trail ${position.config.trailingPct}%` : ""}</div>
        <div className="mono metric-value">{fmt(position?.currentSl ?? null)}</div>
        <div className="metric-sub mono">
          {position?.config.trailingPct
            ? position.config.side === "long"
              ? `peak ${fmt(position.peakPrice)}`
              : `trough ${fmt(position.peakPrice)}`
            : position?.config.sl !== null && position?.config.sl !== undefined
            ? "fixed"
            : "not set"}
        </div>
      </div>
    </div>
  );
}
