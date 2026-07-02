"use client";

import type { Tick } from "@/lib/store";
import type { RiskAlertState, Alert } from "@/lib/riskalert-engine";

type Props = Readonly<{ ticks: readonly Tick[]; riskalert: RiskAlertState | null }>;

const W = 720;
const ROW_H = 76;
const GAUGE_H = 22;
const PAD_L = 160;
const PAD_R = 100;
const TIMELINE_H = 48;

function fmt(n: number): string {
  if (!Number.isFinite(n)) return "—";
  const abs = Math.abs(n);
  if (abs >= 1000) return n.toFixed(0);
  if (abs >= 1) return n.toFixed(1);
  return n.toFixed(3);
}

type Row = { label: string; value: number; threshold: number; kind: string };

export function RiskAlertChart({ ticks, riskalert }: Props) {
  if (!riskalert || ticks.length === 0) {
    return <div style={{ height: 260, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start Risk Alert to see gauge levels and alert timeline.</div>;
  }
  const rows: Row[] = [
    { label: "open orders", value: riskalert.openOrders, threshold: riskalert.config.maxOpenOrders, kind: "open-orders" },
    { label: "failed settlements", value: riskalert.failedSettlements, threshold: riskalert.config.maxFailedSettlements, kind: "failed-settlements" },
    { label: "open notional", value: riskalert.openNotional, threshold: riskalert.config.maxOpenNotional, kind: "open-notional" },
  ];

  const totalH = rows.length * ROW_H + TIMELINE_H + 40;
  const barW = W - PAD_L - PAD_R;

  // Timeline for alerts
  const tMin = ticks[0].t;
  const tMax = ticks[ticks.length - 1].t;
  const tRange = Math.max(1, tMax - tMin);
  const alerts: readonly Alert[] = riskalert.alerts;

  return (
    <svg viewBox={`0 0 ${W} ${totalH}`} style={{ width: "100%", height: "auto", maxHeight: 360 }}>
      {rows.map((r, i) => {
        const y = i * ROW_H + 12;
        const ratio = r.threshold > 0 ? r.value / r.threshold : 0;
        // full bar shows 0..150% of threshold; anything above 100% goes red
        const displayRatio = Math.min(ratio, 1.5);
        const fillW = (displayRatio / 1.5) * barW;
        const thresholdX = PAD_L + (1 / 1.5) * barW;
        const breached = ratio > 1;
        const color = breached ? "var(--neg, #ff6b6b)" : ratio > 0.75 ? "#e0b25a" : "var(--pos, #6de6a3)";
        const bgColor = "#1a2340";

        return (
          <g key={r.kind}>
            <text x={16} y={y + GAUGE_H / 2 + 4} fill="var(--text-faint)" fontSize={12} fontFamily="ui-monospace, monospace">{r.label}</text>
            <rect x={PAD_L} y={y} width={barW} height={GAUGE_H} rx={4} fill={bgColor} />
            <rect x={PAD_L} y={y} width={Math.max(0, fillW)} height={GAUGE_H} rx={4} fill={color} opacity={0.85} />
            {/* threshold marker */}
            <line x1={thresholdX} x2={thresholdX} y1={y - 3} y2={y + GAUGE_H + 3} stroke="#ffb547" strokeWidth={1.5} strokeDasharray="3,2" />
            <text x={thresholdX} y={y - 6} fill="#ffb547" fontSize={9} textAnchor="middle" fontFamily="ui-monospace, monospace">limit {fmt(r.threshold)}</text>
            {/* value labels */}
            <text x={PAD_L + barW + 8} y={y + GAUGE_H / 2 + 4} fill={breached ? "var(--neg, #ff6b6b)" : "#ececf1"} fontSize={12} fontFamily="ui-monospace, monospace">
              {fmt(r.value)} ({(ratio * 100).toFixed(0)}%)
            </text>
            <text x={PAD_L} y={y + GAUGE_H + 14} fill="var(--text-faint)" fontSize={9} fontFamily="ui-monospace, monospace">0</text>
          </g>
        );
      })}

      {/* Timeline of alert events */}
      <g transform={`translate(0, ${rows.length * ROW_H + 8})`}>
        <text x={16} y={16} fill="var(--text-faint)" fontSize={11} fontFamily="ui-monospace, monospace">alerts timeline</text>
        <rect x={PAD_L} y={4} width={barW} height={TIMELINE_H - 8} rx={4} fill="#101a30" />
        {alerts.map((a) => {
          const x = PAD_L + ((a.t - tMin) / tRange) * barW;
          const color = a.cleared ? "#6de6a3" : "#ffb547";
          return (
            <line key={`${a.seq}-${a.t}-${a.cleared ? "ok" : "al"}`} x1={x} x2={x} y1={4} y2={TIMELINE_H - 4} stroke={color} strokeWidth={1.5} opacity={0.9} />
          );
        })}
        <text x={PAD_L} y={TIMELINE_H + 6} fill="var(--text-faint)" fontSize={9} fontFamily="ui-monospace, monospace">t₀</text>
        <text x={PAD_L + barW} y={TIMELINE_H + 6} fill="var(--text-faint)" fontSize={9} textAnchor="end" fontFamily="ui-monospace, monospace">now</text>
        <text x={PAD_L + barW + 8} y={TIMELINE_H / 2 + 4} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">{alerts.length} evts</text>
      </g>
    </svg>
  );
}
