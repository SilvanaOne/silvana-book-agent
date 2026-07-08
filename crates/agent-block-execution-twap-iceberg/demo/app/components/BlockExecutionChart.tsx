"use client";

import type { Tick } from "@/lib/store";
import type { BlockExecutionState, BlockOrder } from "@/lib/blockexecution-engine";

type Props = Readonly<{ ticks: readonly Tick[]; blockexecution: BlockExecutionState | null }>;

const W = 720, H = 300, PAD_L = 60, PAD_R = 20, PAD_T = 14, PAD_B = 32;

export function BlockExecutionChart({ ticks, blockexecution }: Props) {
  if (ticks.length === 0 || !blockexecution) {
    return (
      <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>
        No data — start Block Execution to see mid, limit price, slice boundaries and chunks.
      </div>
    );
  }
  const config = blockexecution.config;
  const prices = ticks.map((t) => t.price);
  const orderPrices = blockexecution.orders.map((o) => o.price);

  const allVals = [...prices, config.price, ...orderPrices];
  const minAll = Math.min(...allVals);
  const maxAll = Math.max(...allVals);
  const pad = (maxAll - minAll) * 0.15 || minAll * 0.02;
  const yMin = minAll - pad, yMax = maxAll + pad, yRange = yMax - yMin || 1;

  const startedAt = blockexecution.startedAt ?? ticks[0].t;
  const scheduleEnd = startedAt + config.timeSlices * blockexecution.sliceIntervalMs;
  const tMin = Math.min(ticks[0].t, startedAt);
  const tMax = Math.max(ticks[ticks.length - 1].t, scheduleEnd);
  const tRange = tMax - tMin || 1;

  const x = (t: number) => PAD_L + ((t - tMin) / tRange) * (W - PAD_L - PAD_R);
  const y = (v: number) => PAD_T + (1 - (v - yMin) / yRange) * (H - PAD_T - PAD_B);

  const pricePath = ticks
    .map((t, i) => `${i === 0 ? "M" : "L"}${x(t.t).toFixed(1)},${y(t.price).toFixed(1)}`)
    .join(" ");

  const digits = yMax > 100 ? 2 : yMax > 1 ? 4 : 6;
  const yTicks: number[] = [];
  for (let i = 0; i <= 4; i++) yTicks.push(yMin + (yRange * i) / 4);

  // Slice boundaries as vertical dashed lines.
  const boundaries: { t: number; idx: number }[] = [];
  for (let i = 0; i <= config.timeSlices; i++) {
    boundaries.push({ t: startedAt + i * blockexecution.sliceIntervalMs, idx: i });
  }

  const lastPrice = ticks[ticks.length - 1].price;

  // Progress bar (below chart)
  const totalFilled = blockexecution.totalFilled;
  const total = config.total;
  const pctFilled = total > 0 ? Math.min(1, totalFilled / total) : 0;
  const barY = H - PAD_B + 12;
  const barX = PAD_L;
  const barW = W - PAD_L - PAD_R;
  const barH = 10;

  return (
    <svg viewBox={`0 0 ${W + 90} ${H + 40}`} style={{ width: "100%", height: "auto", maxHeight: 380 }}>
      <rect x={0} y={0} width={W + 90} height={H + 40} fill="transparent" />
      {yTicks.map((v, i) => (
        <g key={i}>
          <line x1={PAD_L} x2={W - PAD_R} y1={y(v)} y2={y(v)} stroke="#22222c" strokeWidth={1} />
          <text x={PAD_L - 8} y={y(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">
            {v.toFixed(digits)}
          </text>
        </g>
      ))}

      {/* Slice boundary dashed verticals + labels */}
      {boundaries.map((b, i) => {
        if (b.t < tMin || b.t > tMax) return null;
        const xb = x(b.t);
        const isActive = i === blockexecution.currentSlice - 1;
        return (
          <g key={i}>
            <line x1={xb} x2={xb} y1={PAD_T} y2={H - PAD_B} stroke={isActive ? "var(--accent)" : "#3a3a48"} strokeDasharray="3,4" strokeWidth={isActive ? 1.3 : 1} opacity={isActive ? 0.75 : 0.55} />
            {i < config.timeSlices && (
              <text x={xb + 3} y={PAD_T + 10} fill="var(--text-faint)" fontSize={9} fontFamily="ui-monospace, monospace">
                s{i + 1}
              </text>
            )}
          </g>
        );
      })}

      {/* Limit price horizontal line */}
      <line x1={PAD_L} x2={W - PAD_R} y1={y(config.price)} y2={y(config.price)} stroke="var(--accent)" strokeDasharray="4,3" strokeWidth={1.4} opacity={0.85} />

      {/* Mid price path */}
      <path d={pricePath} stroke="#ececf1" strokeWidth={1.6} fill="none" />

      {/* Chunks: circle for open, filled disc for filled */}
      {blockexecution.orders.map((o: BlockOrder) => {
        const color = o.side === "BID" ? "var(--pos)" : "var(--neg)";
        const fillCol = o.status === "filled" ? color : "var(--bg-card)";
        return (
          <g key={o.seq}>
            <circle cx={x(o.t)} cy={y(o.price)} r={4.5} fill={fillCol} stroke={color} strokeWidth={1.6} />
          </g>
        );
      })}

      {/* Current price dot */}
      <circle cx={x(ticks[ticks.length - 1].t)} cy={y(lastPrice)} r={3.5} fill="#ececf1" />

      {/* Right-margin label: limit price */}
      <text x={W - PAD_R + 4} y={y(config.price) + 3} fill="var(--accent)" fontSize={10} fontFamily="ui-monospace, monospace">
        LIMIT {config.price.toFixed(digits)}
      </text>

      {/* Progress bar */}
      <rect x={barX} y={barY} width={barW} height={barH} fill="#22222c" rx={2} />
      <rect x={barX} y={barY} width={barW * pctFilled} height={barH} fill="var(--accent)" rx={2} />
      <text x={barX} y={barY + barH + 12} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">
        parent filled {totalFilled.toFixed(4)} / {total.toFixed(4)} ({(pctFilled * 100).toFixed(1)}%) · slice {blockexecution.currentSlice}/{config.timeSlices} · chunks placed {blockexecution.chunksPlaced} · filled {blockexecution.chunksFilled}
      </text>
    </svg>
  );
}
