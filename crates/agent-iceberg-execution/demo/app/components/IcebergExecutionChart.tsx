"use client";

import type { Tick } from "@/lib/store";
import type { IcebergExecutionState, IcebergOrder } from "@/lib/icebergexecution-engine";

type Props = Readonly<{ ticks: readonly Tick[]; icebergexecution: IcebergExecutionState | null }>;

const W = 720, H = 300, PAD_L = 60, PAD_R = 60, PAD_T = 14, PAD_B = 26;
const BAR_H = 22;
const BAR_TOP_GAP = 18;

export function IcebergExecutionChart({ ticks, icebergexecution }: Props) {
  if (ticks.length === 0 || !icebergexecution) {
    return <div style={{ height: H + BAR_H + BAR_TOP_GAP, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start Iceberg to see mid, limit price, chunk placements and progress.</div>;
  }
  const prices = ticks.map((t) => t.price);
  const limitPrice = icebergexecution.config.price;
  const orderPrices = icebergexecution.orders.map((o) => o.price);

  const minAll = Math.min(...prices, limitPrice, ...orderPrices);
  const maxAll = Math.max(...prices, limitPrice, ...orderPrices);
  const pad = (maxAll - minAll) * 0.1 || minAll * 0.02;
  const yMin = minAll - pad, yMax = maxAll + pad, yRange = yMax - yMin || 1;
  const tMin = ticks[0].t, tMax = ticks[ticks.length - 1].t;
  const tRange = tMax - tMin || 1;
  const x = (t: number) => PAD_L + ((t - tMin) / tRange) * (W - PAD_L - PAD_R);
  const y = (v: number) => PAD_T + (1 - (v - yMin) / yRange) * (H - PAD_T - PAD_B);

  const pricePath = ticks.map((t, i) => `${i === 0 ? "M" : "L"}${x(t.t).toFixed(1)},${y(t.price).toFixed(1)}`).join(" ");

  const digits = yMax > 100 ? 2 : yMax > 1 ? 4 : 6;
  const yTicks: number[] = [];
  for (let i = 0; i <= 4; i++) yTicks.push(yMin + (yRange * i) / 4);

  const lastPrice = ticks[ticks.length - 1].price;
  const totalFilled = icebergexecution.totalFilled;
  const totalTarget = icebergexecution.config.total;
  const pct = Math.max(0, Math.min(1, totalTarget > 0 ? totalFilled / totalTarget : 0));

  const barY = H + BAR_TOP_GAP;
  const barWFull = W - PAD_L - PAD_R;
  const barWFill = barWFull * pct;
  const chartHeight = H + BAR_TOP_GAP + BAR_H + 6;

  return (
    <svg viewBox={`0 0 ${W + 30} ${chartHeight}`} style={{ width: "100%", height: "auto", maxHeight: 420 }}>
      <rect x={0} y={0} width={W + 30} height={chartHeight} fill="transparent" />

      {/* y-axis grid + labels */}
      {yTicks.map((v, i) => (
        <g key={i}>
          <line x1={PAD_L} x2={W - PAD_R} y1={y(v)} y2={y(v)} stroke="#22222c" strokeWidth={1} />
          <text x={PAD_L - 8} y={y(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{v.toFixed(digits)}</text>
        </g>
      ))}

      {/* Horizontal limit price line (orange) */}
      <line x1={PAD_L} x2={W - PAD_R} y1={y(limitPrice)} y2={y(limitPrice)} stroke="var(--accent)" strokeWidth={1.4} strokeDasharray="5,4" />
      <text x={W - PAD_R + 4} y={y(limitPrice) + 3} fill="var(--accent)" fontSize={10} fontFamily="ui-monospace, monospace">price {limitPrice.toFixed(digits)}</text>

      {/* Mid price */}
      <path d={pricePath} stroke="#ececf1" strokeWidth={1.6} fill="none" />

      {/* Chunk placements on the limit line: filled disc if filled, hollow circle if open */}
      {icebergexecution.orders.map((o: IcebergOrder) => {
        const color = o.side === "BID" ? "var(--pos)" : "var(--neg)";
        const fillCol = o.status === "filled" ? color : "var(--bg-card)";
        return (
          <g key={o.seq}>
            <circle cx={x(o.t)} cy={y(o.price)} r={5} fill={fillCol} stroke={color} strokeWidth={1.6} />
          </g>
        );
      })}

      {/* Last mid tick marker */}
      <circle cx={x(tMax)} cy={y(lastPrice)} r={3.5} fill="#ececf1" />

      {/* Legend */}
      <g fontFamily="ui-monospace, monospace" fontSize={10}>
        <rect x={PAD_L} y={PAD_T + 2} width={10} height={2} fill="#ececf1" />
        <text x={PAD_L + 14} y={PAD_T + 6} fill="var(--text-faint)">mid</text>
        <rect x={PAD_L + 60} y={PAD_T + 2} width={10} height={2} fill="var(--accent)" />
        <text x={PAD_L + 74} y={PAD_T + 6} fill="var(--text-faint)">limit price</text>
        <circle cx={PAD_L + 160} cy={PAD_T + 4} r={4} fill="var(--bg-card)" stroke="var(--pos)" strokeWidth={1.4} />
        <text x={PAD_L + 168} y={PAD_T + 8} fill="var(--text-faint)">chunk open</text>
        <circle cx={PAD_L + 240} cy={PAD_T + 4} r={4} fill="var(--pos)" stroke="var(--pos)" strokeWidth={1.4} />
        <text x={PAD_L + 248} y={PAD_T + 8} fill="var(--text-faint)">chunk filled</text>
      </g>

      {/* Progress bar */}
      <g>
        <text x={PAD_L} y={barY - 4} fill="var(--text-faint)" fontSize={11} fontFamily="ui-monospace, monospace">
          progress: {totalFilled.toFixed(4)} / {totalTarget} ({(pct * 100).toFixed(1)}%) — {icebergexecution.chunksFilled}/{icebergexecution.chunksPlaced} chunks
        </text>
        <rect x={PAD_L} y={barY} width={barWFull} height={BAR_H} rx={3} fill="#1a1a22" stroke="#33333e" />
        <rect x={PAD_L} y={barY} width={barWFill} height={BAR_H} rx={3} fill="var(--accent)" opacity={0.85} />
      </g>
    </svg>
  );
}
