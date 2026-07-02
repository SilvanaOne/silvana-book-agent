"use client";

import type { RiskExposureState, Position } from "@/lib/riskexposure-engine";

type Props = Readonly<{ riskexposure: RiskExposureState | null }>;

const W = 720;
const ROW_H = 44;
const PAD_T = 14;
const PAD_B = 28;
const LABEL_W = 90;
const VALUE_W = 130;
const BAR_L = LABEL_W + 12;
const BAR_R = W - VALUE_W - 12;

const PALETTE = ["#4c8bf5", "#f5a623", "#7ed957", "#e15252", "#b46cf0", "#4dd0e1", "#f06292", "#a1887f"];

export function RiskExposureChart({ riskexposure }: Props) {
  if (!riskexposure) {
    return <div style={{ height: 200, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start Risk Exposure to see per-instrument concentration.</div>;
  }
  const positions = riskexposure.positions;
  const total = riskexposure.totalPortfolioQuote;
  const warnPct = riskexposure.config.concentrationWarnPct;
  const H = PAD_T + PAD_B + Math.max(1, positions.length) * ROW_H;
  const barWidth = BAR_R - BAR_L;
  const warnX = BAR_L + (Math.min(100, warnPct) / 100) * barWidth;

  return (
    <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: "auto", maxHeight: H + 20 }}>
      {/* Warn threshold header line */}
      <line x1={warnX} x2={warnX} y1={PAD_T - 6} y2={H - PAD_B + 6} stroke="var(--neg)" strokeWidth={1.2} strokeDasharray="4,3" />
      <text x={warnX + 4} y={PAD_T - 2} fill="var(--neg)" fontSize={10} fontFamily="ui-monospace, monospace">warn {warnPct}%</text>

      {positions.map((p, i) => {
        const y = PAD_T + i * ROW_H + 10;
        const barY = y + 6;
        const sharePct = Math.min(100, p.sharePct);
        const fillW = (sharePct / 100) * barWidth;
        const warned = p.sharePct > warnPct;
        const color = warned ? "var(--neg)" : PALETTE[i % PALETTE.length];
        return (
          <g key={p.instrument}>
            <text x={LABEL_W} y={y + 12} fill="var(--text-strong)" fontSize={13} textAnchor="end" fontFamily="ui-monospace, monospace">{p.instrument}</text>
            <rect x={BAR_L} y={barY} width={barWidth} height={18} rx={4} fill="#1c1c25" stroke="#22222c" strokeWidth={1} />
            <rect x={BAR_L} y={barY} width={fillW} height={18} rx={4} fill={color} opacity={warned ? 0.9 : 0.75} />
            <text x={BAR_L + Math.max(4, fillW - 4)} y={barY + 13} fill={fillW > 44 ? "#0e0e14" : "var(--text-strong)"} fontSize={11} textAnchor={fillW > 44 ? "end" : "start"} fontFamily="ui-monospace, monospace">
              {p.sharePct.toFixed(2)}%
            </text>
            <text x={BAR_R + 8} y={barY + 8} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">bal {fmt(p.balance)}</text>
            <text x={BAR_R + 8} y={barY + 20} fill="var(--text-strong)" fontSize={11} fontFamily="ui-monospace, monospace">${fmt(p.valueQuote)}</text>
          </g>
        );
      })}

      {/* Portfolio total footer */}
      <text x={LABEL_W} y={H - 8} fill="var(--text-faint)" fontSize={11} textAnchor="end" fontFamily="ui-monospace, monospace">TOTAL</text>
      <text x={BAR_L} y={H - 8} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">${fmt(total)} across {positions.length} instruments</text>
    </svg>
  );
}

function fmt(n: number): string {
  const abs = Math.abs(n);
  if (abs >= 1000) return n.toFixed(0);
  if (abs >= 1) return n.toFixed(2);
  return n.toFixed(4);
}

// re-export Position so page can use it if needed elsewhere
export type { Position };
