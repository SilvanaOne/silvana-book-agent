"use client";

import type { PositionState } from "@/lib/tpsl-engine";

type Props = Readonly<{ position: PositionState | null }>;

function fmt(n: number | null, min = 4): string {
  if (n === null) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

export function StatusPanel({ position }: Props) {
  if (!position) {
    return <div className="muted">No position — configure and click "Start monitoring".</div>;
  }
  const cfg = position.config;
  const pnlSign = position.pnl > 0 ? "positive" : position.pnl < 0 ? "negative" : "muted";
  const badge = position.status === "monitoring" ? "monitoring" : position.status === "triggered" ? "triggered" : "idle";

  return (
    <div className="stack">
      <div className="row" style={{ justifyContent: "space-between", alignItems: "baseline", flexWrap: "wrap", gap: 12 }}>
        <div>
          <div className="muted" style={{ fontSize: 12 }}>Position</div>
          <div className="mono" style={{ fontSize: 15 }}>
            <span className={`badge ${cfg.side}`}>{cfg.side.toUpperCase()}</span>{" "}
            <strong>{cfg.quantity}</strong> {cfg.market}
          </div>
        </div>
        <span className={`badge ${badge}`}>{position.status}</span>
      </div>

      <div className="grid-2">
        <div className="card" style={{ padding: 14, background: "#0f1730" }}>
          <div className="muted" style={{ fontSize: 11, textTransform: "uppercase", letterSpacing: 0.5 }}>Current price</div>
          <div className="mono metric-value">{fmt(position.currentPrice)}</div>
        </div>
        <div className="card" style={{ padding: 14, background: "#0f1730" }}>
          <div className="muted" style={{ fontSize: 11, textTransform: "uppercase", letterSpacing: 0.5 }}>Unrealized PnL</div>
          <div className={`mono metric-value ${pnlSign}`}>
            {position.pnl > 0 ? "+" : ""}
            {fmt(position.pnl, 6)}
          </div>
        </div>
      </div>

      <div className="grid-2">
        <div>
          <div className="muted" style={{ fontSize: 12 }}>Entry</div>
          <div className="mono">{fmt(cfg.entryPrice)}</div>
        </div>
        <div>
          <div className="muted" style={{ fontSize: 12 }}>
            {cfg.side === "long" ? "Peak" : "Trough"}
          </div>
          <div className="mono">{fmt(position.peakPrice)}</div>
        </div>
        <div>
          <div className="muted" style={{ fontSize: 12 }}>Take Profit</div>
          <div className="mono">{fmt(cfg.tp)}</div>
        </div>
        <div>
          <div className="muted" style={{ fontSize: 12 }}>
            Stop Loss{cfg.trailingPct ? ` (trailing ${cfg.trailingPct}%)` : ""}
          </div>
          <div className="mono">{fmt(position.currentSl)}</div>
        </div>
      </div>

      {position.status === "triggered" && (
        <div
          className="mono"
          style={{
            padding: 12,
            borderRadius: 6,
            background: position.triggeredReason === "TAKE PROFIT" ? "#1e4a2e" : "#4a1e2e",
            color: position.triggeredReason === "TAKE PROFIT" ? "#6de6a3" : "#ff8dab",
            fontSize: 14,
            fontWeight: 600,
          }}
        >
          {position.triggeredReason} triggered @ {fmt(position.currentPrice)} — real agent would send {cfg.side === "long" ? "SELL" : "BUY"} order for {cfg.quantity} {cfg.market}
        </div>
      )}
    </div>
  );
}
