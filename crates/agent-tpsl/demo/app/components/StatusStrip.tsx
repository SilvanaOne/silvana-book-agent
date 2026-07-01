"use client";

import type { PositionState } from "@/lib/tpsl-engine";

type Props = Readonly<{ position: PositionState | null; tickCount: number }>;

export function StatusStrip({ position, tickCount }: Props) {
  const state = position?.status ?? "idle";
  const label = state === "monitoring" ? "LIVE" : state === "triggered" ? "TRIGGERED" : "OFFLINE";
  const cls = state === "monitoring" ? "live" : state === "triggered" ? "triggered" : "";
  const market = position?.config.market ?? "—";
  const side = position?.config.side;
  return (
    <div className="statusbar">
      <div className="statusbar-inner">
        <span className={`status-pill ${cls}`}><span className="dot" />{label}</span>
        <span className="mono muted">market: <span style={{ color: "var(--text)" }}>{market}</span></span>
        {side && (
          <span className="muted">side: <span className={`badge ${side}`}>{side.toUpperCase()}</span></span>
        )}
        <span className="mono muted">ticks: <span style={{ color: "var(--text)" }}>{tickCount}</span></span>
        <span style={{ marginLeft: "auto" }} className="mono faint">
          engine mirror of crates/agent-tpsl/src/main.rs
        </span>
      </div>
    </div>
  );
}
