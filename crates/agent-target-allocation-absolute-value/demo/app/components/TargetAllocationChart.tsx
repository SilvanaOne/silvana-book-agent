"use client";

import type { Tick } from "@/lib/store";
import type { TargetAllocationState, AllocationOrder } from "@/lib/targetallocation-engine";

type Props = Readonly<{ ticks: readonly Tick[]; targetallocation: TargetAllocationState | null }>;

const W = 720, H = 300, PAD_L = 130, PAD_R = 60, PAD_T = 20, PAD_B = 30;

export function TargetAllocationChart({ ticks, targetallocation }: Props) {
  if (!targetallocation || targetallocation.positions.length === 0) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start Target Allocation to see per-instrument allocation vs target.</div>;
  }

  const positions = targetallocation.positions;
  const n = positions.length;
  const rowH = (H - PAD_T - PAD_B) / n;
  const barH = Math.min(28, rowH * 0.5);

  // xMax is the largest of any current or target across all positions.
  const values = positions.flatMap((p) => [p.currentQuote, p.targetQuote]);
  const xMax = Math.max(1, ...values) * 1.1;

  const barX0 = PAD_L;
  const barX1 = W - PAD_R;
  const barW = barX1 - barX0;
  const x = (v: number) => barX0 + Math.max(0, Math.min(1, v / xMax)) * barW;

  const digits = xMax > 100 ? 0 : xMax > 1 ? 2 : 4;

  // X-axis ticks
  const xTicks = [0, 0.25, 0.5, 0.75, 1.0].map((f) => f * xMax);

  const orderMarkerY = (pos: number) => PAD_T + rowH * pos + rowH / 2;

  return (
    <svg viewBox={`0 0 ${W + 20} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 340 }}>
      <rect x={0} y={0} width={W + 20} height={H} fill="transparent" />

      {/* X-axis */}
      {xTicks.map((v, i) => (
        <g key={i}>
          <line x1={x(v)} x2={x(v)} y1={PAD_T} y2={H - PAD_B} stroke="#22222c" strokeWidth={1} strokeDasharray={i === 0 ? "" : "2,4"} />
          <text x={x(v)} y={H - PAD_B + 12} fill="var(--text-faint)" fontSize={10} textAnchor="middle" fontFamily="ui-monospace, monospace">
            {v.toFixed(digits)}
          </text>
        </g>
      ))}

      {positions.map((p, i) => {
        const cy = PAD_T + rowH * i + rowH / 2;
        const bt = barH;
        const y0 = cy - bt / 2;
        // Target bar (gray outline)
        const targetX = x(p.targetQuote);
        const currentX = x(p.currentQuote);
        const overshoot = p.deviationQuote > 0;
        const undershoot = p.deviationQuote < 0;
        return (
          <g key={p.instrument + p.market}>
            {/* Row label */}
            <text x={PAD_L - 8} y={cy - 4} fill="#ececf1" fontSize={11} textAnchor="end" fontFamily="ui-monospace, monospace">{p.instrument}</text>
            <text x={PAD_L - 8} y={cy + 9} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{p.market}</text>

            {/* Current-quote bar */}
            <rect
              x={barX0}
              y={y0}
              width={Math.max(0, currentX - barX0)}
              height={bt}
              fill={overshoot ? "rgba(255,141,171,0.25)" : undershoot ? "rgba(109,230,163,0.18)" : "rgba(109,230,163,0.35)"}
              stroke={overshoot ? "var(--neg)" : "var(--pos)"}
              strokeWidth={1.2}
            />

            {/* Overshoot segment (red) */}
            {overshoot && (
              <rect
                x={targetX}
                y={y0}
                width={Math.max(0, currentX - targetX)}
                height={bt}
                fill="rgba(255,141,171,0.55)"
                stroke="var(--neg)"
                strokeWidth={1.2}
              />
            )}

            {/* Target marker line */}
            <line x1={targetX} x2={targetX} y1={y0 - 4} y2={y0 + bt + 4} stroke="#ececf1" strokeWidth={1.6} />
            <text x={targetX} y={y0 - 6} fill="#ececf1" fontSize={9} textAnchor="middle" fontFamily="ui-monospace, monospace">target</text>

            {/* Current value label */}
            <text x={currentX + 4} y={cy + 4} fill={overshoot ? "var(--neg)" : "var(--pos)"} fontSize={10} fontFamily="ui-monospace, monospace">
              {p.currentQuote.toFixed(digits)}
            </text>

            {/* Order markers on the row */}
            {targetallocation.orders
              .filter((o: AllocationOrder) => o.instrument === p.instrument && o.market === p.market)
              .slice(-8)
              .map((o: AllocationOrder, idx: number) => {
                const color = o.side === "BID" ? "var(--pos)" : "var(--neg)";
                const fillCol = o.status === "filled" ? color : "var(--bg-card)";
                // Fan out markers vertically so multiple orders stay legible.
                const dy = ((idx % 5) - 2) * 3;
                const mx = x(o.quoteAmount);
                return (
                  <g key={o.seq}>
                    <circle cx={mx} cy={orderMarkerY(i) + dy} r={4} fill={fillCol} stroke={color} strokeWidth={1.5} />
                  </g>
                );
              })}
          </g>
        );
      })}

      {/* Legend */}
      <g transform={`translate(${PAD_L}, ${H - 12})`}>
        <rect x={0} y={-8} width={10} height={8} fill="rgba(109,230,163,0.35)" stroke="var(--pos)" />
        <text x={14} y={0} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">current</text>
        <rect x={70} y={-8} width={10} height={8} fill="rgba(255,141,171,0.55)" stroke="var(--neg)" />
        <text x={84} y={0} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">overshoot</text>
        <line x1={160} x2={160} y1={-10} y2={2} stroke="#ececf1" strokeWidth={1.6} />
        <text x={166} y={0} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">target</text>
        <circle cx={220} cy={-4} r={4} fill="var(--pos)" stroke="var(--pos)" />
        <text x={228} y={0} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">BID</text>
        <circle cx={260} cy={-4} r={4} fill="var(--neg)" stroke="var(--neg)" />
        <text x={268} y={0} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">OFFER</text>
      </g>

      {/* base price / axis title */}
      <text x={W - PAD_R} y={PAD_T - 6} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">
        base price {targetallocation.currentPrice.toFixed(4)}
      </text>
    </svg>
  );
}
