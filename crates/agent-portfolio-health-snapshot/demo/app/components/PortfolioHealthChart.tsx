"use client";

import type { Tick } from "@/lib/store";
import type { PortfolioHealthState } from "@/lib/portfoliohealth-engine";

type Props = Readonly<{ ticks: readonly Tick[]; portfoliohealth: PortfolioHealthState | null }>;

const W = 720;
const H_TOP = 150;          // balance bar chart
const H_BOTTOM = 170;       // portfolio-value line chart
const H = H_TOP + H_BOTTOM + 30; // total height incl. gap
const PAD_L = 60;
const PAD_R = 20;
const PAD_T = 14;
const PAD_B = 26;

export function PortfolioHealthChart({ ticks, portfoliohealth }: Props) {
  if (!portfoliohealth || ticks.length === 0) {
    return (
      <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>
        No data — start Portfolio Health to see balances and portfolio value.
      </div>
    );
  }

  // -------------- Top: balances bar chart --------------
  const bals = portfoliohealth.balances;
  const maxBal = Math.max(1, ...bals.map((b) => b.balance));
  const barTopY0 = PAD_T;
  const barBottomY = H_TOP - PAD_B / 2;
  const barAreaH = barBottomY - barTopY0;
  const barAreaW = W - PAD_L - PAD_R;
  const barSlot = bals.length > 0 ? barAreaW / bals.length : barAreaW;
  const barW = Math.min(60, barSlot * 0.55);

  const bars = bals.map((b, i) => {
    const cx = PAD_L + barSlot * (i + 0.5);
    const h = (b.balance / maxBal) * barAreaH;
    const y = barBottomY - h;
    return { b, cx, y, h };
  });

  // -------------- Bottom: portfolio value line chart --------------
  const pvs = ticks.map((t) => t.portfolioValue);
  const pvMin = Math.min(...pvs);
  const pvMax = Math.max(...pvs);
  const pvPad = (pvMax - pvMin) * 0.1 || pvMin * 0.02 || 1;
  const yMin = pvMin - pvPad;
  const yMax = pvMax + pvPad;
  const yRange = yMax - yMin || 1;

  const tMin = ticks[0].t;
  const tMax = ticks[ticks.length - 1].t;
  const tRange = tMax - tMin || 1;

  const lineTopY = H_TOP + 30;
  const lineBottomY = H - PAD_B;
  const lineH = lineBottomY - lineTopY;
  const lineW = W - PAD_L - PAD_R;

  const xLine = (t: number) => PAD_L + ((t - tMin) / tRange) * lineW;
  const yLine = (v: number) => lineTopY + (1 - (v - yMin) / yRange) * lineH;

  const pvPath = ticks
    .map((t, i) => `${i === 0 ? "M" : "L"}${xLine(t.t).toFixed(1)},${yLine(t.portfolioValue).toFixed(1)}`)
    .join(" ");

  const yTicks: number[] = [];
  for (let i = 0; i <= 4; i++) yTicks.push(yMin + (yRange * i) / 4);

  const digits = yMax > 100 ? 2 : yMax > 1 ? 4 : 6;
  const lastPv = ticks[ticks.length - 1].portfolioValue;

  return (
    <svg viewBox={`0 0 ${W + 90} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 420 }}>
      {/* Top section title */}
      <text x={PAD_L} y={12} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">BALANCES (unlocked)</text>

      {/* Balances bar chart */}
      <line x1={PAD_L} x2={W - PAD_R} y1={barBottomY} y2={barBottomY} stroke="#22222c" strokeWidth={1} />
      {bars.map(({ b, cx, y, h }) => (
        <g key={b.instrument}>
          <rect
            x={cx - barW / 2}
            y={y}
            width={barW}
            height={Math.max(0.5, h)}
            fill="var(--accent)"
            opacity={0.9}
          />
          <text
            x={cx}
            y={y - 5}
            fill="var(--text)"
            fontSize={11}
            textAnchor="middle"
            fontFamily="ui-monospace, monospace"
          >
            {fmtBal(b.balance)}
          </text>
          <text
            x={cx}
            y={barBottomY + 14}
            fill="var(--text-faint)"
            fontSize={11}
            textAnchor="middle"
            fontFamily="ui-monospace, monospace"
          >
            {b.instrument}
          </text>
        </g>
      ))}

      {/* Separator */}
      <line x1={0} x2={W + 90} y1={H_TOP + 12} y2={H_TOP + 12} stroke="#1c2647" strokeWidth={1} />
      <text x={PAD_L} y={H_TOP + 24} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">
        PORTFOLIO VALUE (quote)
      </text>

      {/* Portfolio value grid + line */}
      {yTicks.map((v, i) => (
        <g key={i}>
          <line x1={PAD_L} x2={W - PAD_R} y1={yLine(v)} y2={yLine(v)} stroke="#22222c" strokeWidth={1} />
          <text x={PAD_L - 8} y={yLine(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">
            {v.toFixed(digits)}
          </text>
        </g>
      ))}
      <path d={pvPath} stroke="var(--accent)" strokeWidth={1.6} fill="none" />
      <circle cx={xLine(tMax)} cy={yLine(lastPv)} r={3.5} fill="var(--accent)" />
      <text x={W - PAD_R + 4} y={yLine(lastPv) + 3} fill="var(--accent)" fontSize={10} fontFamily="ui-monospace, monospace">
        pv {lastPv.toFixed(digits)}
      </text>
    </svg>
  );
}

function fmtBal(n: number): string {
  const abs = Math.abs(n);
  if (abs === 0) return "0";
  if (abs >= 100) return n.toFixed(1);
  if (abs >= 1) return n.toFixed(2);
  return n.toFixed(4);
}
