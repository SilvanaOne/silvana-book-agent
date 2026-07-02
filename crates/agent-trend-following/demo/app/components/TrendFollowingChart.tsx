"use client";

import type { Tick } from "@/lib/store";
import type { TrendFollowingState, TrendFollowingOrder } from "@/lib/trendfollowing-engine";

type Props = Readonly<{ ticks: readonly Tick[]; trendfollowing: TrendFollowingState | null }>;

const W = 720, H = 300, PAD_L = 60, PAD_R = 20, PAD_T = 14, PAD_B = 26;

export function TrendFollowingChart({ ticks, trendfollowing }: Props) {
  if (ticks.length === 0 || !trendfollowing) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start Trend Following to see mid, fast/slow EMAs and crossover signals.</div>;
  }
  const prices = ticks.map((t) => t.price);
  const fastEmas = ticks.map((t) => t.emaFast).filter((v): v is number => v !== null);
  const slowEmas = ticks.map((t) => t.emaSlow).filter((v): v is number => v !== null);
  const orderPrices = trendfollowing.orders.map((o) => o.price);

  const minAll = Math.min(...prices, ...fastEmas, ...slowEmas, ...orderPrices);
  const maxAll = Math.max(...prices, ...fastEmas, ...slowEmas, ...orderPrices);
  const pad = (maxAll - minAll) * 0.1 || minAll * 0.02;
  const yMin = minAll - pad, yMax = maxAll + pad, yRange = yMax - yMin || 1;
  const tMin = ticks[0].t, tMax = ticks[ticks.length - 1].t;
  const tRange = tMax - tMin || 1;
  const x = (t: number) => PAD_L + ((t - tMin) / tRange) * (W - PAD_L - PAD_R);
  const y = (v: number) => PAD_T + (1 - (v - yMin) / yRange) * (H - PAD_T - PAD_B);

  const pricePath = ticks.map((t, i) => `${i === 0 ? "M" : "L"}${x(t.t).toFixed(1)},${y(t.price).toFixed(1)}`).join(" ");
  const fastPath = ticks
    .map((t, i) => t.emaFast === null ? null : `${i === 0 ? "M" : "L"}${x(t.t).toFixed(1)},${y(t.emaFast).toFixed(1)}`)
    .filter((s): s is string => s !== null)
    .join(" ");
  const slowPath = ticks
    .map((t, i) => t.emaSlow === null ? null : `${i === 0 ? "M" : "L"}${x(t.t).toFixed(1)},${y(t.emaSlow).toFixed(1)}`)
    .filter((s): s is string => s !== null)
    .join(" ");

  const digits = yMax > 100 ? 2 : yMax > 1 ? 4 : 6;
  const yTicks: number[] = [];
  for (let i = 0; i <= 4; i++) yTicks.push(yMin + (yRange * i) / 4);

  const lastPrice = ticks[ticks.length - 1].price;
  const lastCrossAt = trendfollowing.lastCrossAt;
  const crossInRange = lastCrossAt !== null && lastCrossAt >= tMin && lastCrossAt <= tMax;
  const crossColor = trendfollowing.lastCross === "up" ? "var(--pos)" : trendfollowing.lastCross === "down" ? "var(--neg)" : "#555";

  return (
    <svg viewBox={`0 0 ${W + 90} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 340 }}>
      <rect x={0} y={0} width={W + 90} height={H} fill="transparent" />
      {yTicks.map((v, i) => (
        <g key={i}>
          <line x1={PAD_L} x2={W - PAD_R} y1={y(v)} y2={y(v)} stroke="#22222c" strokeWidth={1} />
          <text x={PAD_L - 8} y={y(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{v.toFixed(digits)}</text>
        </g>
      ))}

      {/* Last-crossover vertical marker */}
      {crossInRange && lastCrossAt !== null && (
        <line x1={x(lastCrossAt)} x2={x(lastCrossAt)} y1={PAD_T} y2={H - PAD_B} stroke={crossColor} strokeWidth={1} strokeDasharray="4,4" opacity={0.55} />
      )}

      {/* Slow EMA (dim blue) */}
      {slowPath && <path d={slowPath} stroke="#5b78b3" strokeWidth={1.4} fill="none" opacity={0.75} />}

      {/* Fast EMA (accent orange) */}
      {fastPath && <path d={fastPath} stroke="var(--accent)" strokeWidth={1.6} fill="none" opacity={0.95} />}

      {/* Mid price */}
      <path d={pricePath} stroke="#ececf1" strokeWidth={1.6} fill="none" />

      {/* Orders: outlined circle for open, filled disc for filled */}
      {trendfollowing.orders.map((o: TrendFollowingOrder) => {
        const color = o.type === "BID" ? "var(--pos)" : "var(--neg)";
        const fillCol = o.status === "filled" ? color : "var(--bg-card)";
        return (
          <g key={o.seq}>
            <circle cx={x(o.t)} cy={y(o.price)} r={4.5} fill={fillCol} stroke={color} strokeWidth={1.6} />
          </g>
        );
      })}

      <circle cx={x(tMax)} cy={y(lastPrice)} r={3.5} fill="#ececf1" />

      {/* Right-margin labels */}
      {trendfollowing.emaFast !== null && (
        <text x={W - PAD_R + 4} y={y(trendfollowing.emaFast) + 3} fill="var(--accent)" fontSize={10} fontFamily="ui-monospace, monospace">fast {trendfollowing.emaFast.toFixed(digits)}</text>
      )}
      {trendfollowing.emaSlow !== null && (
        <text x={W - PAD_R + 4} y={y(trendfollowing.emaSlow) + 3} fill="#5b78b3" fontSize={10} fontFamily="ui-monospace, monospace">slow {trendfollowing.emaSlow.toFixed(digits)}</text>
      )}
    </svg>
  );
}
