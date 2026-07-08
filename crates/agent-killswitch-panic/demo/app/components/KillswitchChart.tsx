"use client";

import type { Tick } from "@/lib/store";
import type { KillswitchState } from "@/lib/killswitch-engine";

type Props = Readonly<{ ticks: readonly Tick[]; killswitch: KillswitchState | null }>;

const W = 720, H = 300, PAD_L = 60, PAD_R = 60, PAD_T = 14, PAD_B = 26;

export function KillswitchChart({ ticks, killswitch }: Props) {
  if (ticks.length === 0 || !killswitch) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — arm Killswitch to see open orders and failed settlements track their limits.</div>;
  }

  // ticks[].price carries open orders, ticks[].ema carries failed settlements
  const openSeries = ticks.map((t) => t.price);
  const failSeries = ticks.map((t) => (t.ema ?? 0));
  const maxOpen = killswitch.config.maxOpenOrders;
  const maxFail = killswitch.config.maxFailedSettlements;

  // Left axis: open orders. Include threshold in domain.
  const oMin = 0;
  const oMax = Math.max(maxOpen * 1.15, ...openSeries) || maxOpen || 1;

  // Right axis: failed settlements. Include threshold + a small headroom.
  const fMin = 0;
  const fMax = Math.max(maxFail + 2, ...failSeries) || maxFail + 2 || 1;

  const tMin = ticks[0].t, tMax = ticks[ticks.length - 1].t;
  const tRange = tMax - tMin || 1;
  const x = (t: number) => PAD_L + ((t - tMin) / tRange) * (W - PAD_L - PAD_R);
  const yL = (v: number) => PAD_T + (1 - (v - oMin) / (oMax - oMin || 1)) * (H - PAD_T - PAD_B);
  const yR = (v: number) => PAD_T + (1 - (v - fMin) / (fMax - fMin || 1)) * (H - PAD_T - PAD_B);

  const openPath = ticks.map((t, i) => `${i === 0 ? "M" : "L"}${x(t.t).toFixed(1)},${yL(t.price).toFixed(1)}`).join(" ");
  const failPath = ticks.map((t, i) => `${i === 0 ? "M" : "L"}${x(t.t).toFixed(1)},${yR(t.ema ?? 0).toFixed(1)}`).join(" ");

  // Left-axis ticks (open orders).
  const yTicksL: number[] = [];
  for (let i = 0; i <= 4; i++) yTicksL.push(oMin + ((oMax - oMin) * i) / 4);
  // Right-axis ticks (failed).
  const yTicksR: number[] = [];
  for (let i = 0; i <= 4; i++) yTicksR.push(fMin + ((fMax - fMin) * i) / 4);

  const lastOpen = openSeries[openSeries.length - 1];
  const lastFail = failSeries[failSeries.length - 1];

  // TRIP overlay
  const tripT = killswitch.tripTimestamp;
  const tripX = tripT !== null ? x(Math.max(tMin, Math.min(tMax, tripT))) : null;

  return (
    <svg viewBox={`0 0 ${W + 20} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 340 }}>
      <rect x={0} y={0} width={W + 20} height={H} fill="transparent" />

      {/* Left axis gridlines + labels (open orders) */}
      {yTicksL.map((v, i) => (
        <g key={`l${i}`}>
          <line x1={PAD_L} x2={W - PAD_R} y1={yL(v)} y2={yL(v)} stroke="#22222c" strokeWidth={1} />
          <text x={PAD_L - 8} y={yL(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{v.toFixed(0)}</text>
        </g>
      ))}

      {/* Right axis labels (failed) */}
      {yTicksR.map((v, i) => (
        <text key={`r${i}`} x={W - PAD_R + 8} y={yR(v) + 3} fill="var(--neg)" fontSize={10} fontFamily="ui-monospace, monospace" opacity={0.75}>{v.toFixed(0)}</text>
      ))}

      {/* Threshold lines (dashed red) */}
      <line x1={PAD_L} x2={W - PAD_R} y1={yL(maxOpen)} y2={yL(maxOpen)} stroke="var(--neg)" strokeWidth={1.2} strokeDasharray="4,4" opacity={0.9} />
      <text x={W - PAD_R + 4} y={yL(maxOpen) - 3} fill="var(--neg)" fontSize={10} fontFamily="ui-monospace, monospace">MAX {maxOpen}</text>

      <line x1={PAD_L} x2={W - PAD_R} y1={yR(maxFail)} y2={yR(maxFail)} stroke="var(--neg)" strokeWidth={1} strokeDasharray="2,3" opacity={0.75} />

      {/* Open orders (white) */}
      <path d={openPath} stroke="#ececf1" strokeWidth={1.8} fill="none" />
      {/* Failed settlements (thin red) */}
      <path d={failPath} stroke="var(--neg)" strokeWidth={1.2} fill="none" opacity={0.9} />

      {/* Last-value dots */}
      <circle cx={x(tMax)} cy={yL(lastOpen)} r={3.5} fill="#ececf1" />
      <circle cx={x(tMax)} cy={yR(lastFail)} r={3} fill="var(--neg)" />

      {/* TRIP marker */}
      {tripX !== null && (
        <g>
          <line x1={tripX} x2={tripX} y1={PAD_T} y2={H - PAD_B} stroke="var(--neg)" strokeWidth={2} opacity={0.85} />
          <g transform={`translate(${tripX}, ${PAD_T + 18})`}>
            <line x1={-9} y1={-9} x2={9} y2={9} stroke="var(--neg)" strokeWidth={3} />
            <line x1={9} y1={-9} x2={-9} y2={9} stroke="var(--neg)" strokeWidth={3} />
          </g>
          <text x={tripX + 6} y={PAD_T + 42} fill="var(--neg)" fontSize={11} fontWeight={700} fontFamily="ui-monospace, monospace">TRIPPED</text>
        </g>
      )}

      {/* Legend */}
      <g transform={`translate(${PAD_L}, ${H - 6})`} fontSize={10} fontFamily="ui-monospace, monospace">
        <line x1={0} y1={-4} x2={16} y2={-4} stroke="#ececf1" strokeWidth={2} />
        <text x={20} y={-1} fill="var(--text-faint)">open orders</text>
        <line x1={110} y1={-4} x2={126} y2={-4} stroke="var(--neg)" strokeWidth={2} />
        <text x={130} y={-1} fill="var(--text-faint)">failed settlements</text>
      </g>
    </svg>
  );
}
