"use client";

import type { Tick } from "@/lib/store";
import type { TwapState, TwapOrder } from "@/lib/twap-engine";

type Props = Readonly<{ ticks: readonly Tick[]; twap: TwapState | null }>;

const W = 720, H = 300, PAD_L = 60, PAD_R = 20, PAD_T = 14, PAD_B = 26;

export function TwapChart({ ticks, twap }: Props) {
  if (ticks.length === 0 || !twap) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start TWAP to see mid, slice schedule and placed orders.</div>;
  }
  const prices = ticks.map((t) => t.price);
  const orderPrices = twap.orders.map((o) => o.price);
  const limitArr = twap.config.limitPrice !== null ? [twap.config.limitPrice] : [];

  const minAll = Math.min(...prices, ...orderPrices, ...limitArr);
  const maxAll = Math.max(...prices, ...orderPrices, ...limitArr);
  const pad = (maxAll - minAll) * 0.1 || minAll * 0.02;
  const yMin = minAll - pad, yMax = maxAll + pad, yRange = yMax - yMin || 1;

  const tMin = ticks[0].t;
  // Extend x-axis to the end of the TWAP window so future slice marks fit.
  const twapEnd = twap.startTime + twap.config.durationSecs * 1000;
  const tMax = Math.max(ticks[ticks.length - 1].t, twapEnd);
  const tRange = tMax - tMin || 1;
  const x = (t: number) => PAD_L + ((t - tMin) / tRange) * (W - PAD_L - PAD_R);
  const y = (v: number) => PAD_T + (1 - (v - yMin) / yRange) * (H - PAD_T - PAD_B);

  const pricePath = ticks.map((t, i) => `${i === 0 ? "M" : "L"}${x(t.t).toFixed(1)},${y(t.price).toFixed(1)}`).join(" ");

  const digits = yMax > 100 ? 2 : yMax > 1 ? 4 : 6;
  const yTicks: number[] = [];
  for (let i = 0; i <= 4; i++) yTicks.push(yMin + (yRange * i) / 4);

  const lastPrice = ticks[ticks.length - 1].price;

  // Scheduled slice marks: from slice 1 to slices, at startTime + k*intervalMs.
  // Placed slices: solid vertical line at the actual placement time (order.t).
  // Future slices: dashed vertical line at the scheduled instant.
  const scheduled: { k: number; t: number; placed: boolean }[] = [];
  for (let k = 1; k <= twap.config.slices; k++) {
    scheduled.push({ k, t: twap.startTime + k * twap.intervalMs, placed: k <= twap.slicesPlaced });
  }

  return (
    <svg viewBox={`0 0 ${W + 90} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 340 }}>
      <rect x={0} y={0} width={W + 90} height={H} fill="transparent" />
      {yTicks.map((v, i) => (
        <g key={i}>
          <line x1={PAD_L} x2={W - PAD_R} y1={y(v)} y2={y(v)} stroke="#22222c" strokeWidth={1} />
          <text x={PAD_L - 8} y={y(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{v.toFixed(digits)}</text>
        </g>
      ))}

      {/* Scheduled slice grid: dashed vertical for pending, faint solid past-line for placed */}
      {scheduled.map((s) => (
        <line
          key={s.k}
          x1={x(s.t)}
          x2={x(s.t)}
          y1={PAD_T}
          y2={H - PAD_B}
          stroke={s.placed ? "#2b2b36" : "#3f5b8a"}
          strokeWidth={1}
          strokeDasharray={s.placed ? undefined : "3,3"}
          opacity={s.placed ? 0.5 : 0.6}
        />
      ))}

      {/* Optional limit price horizontal line */}
      {twap.config.limitPrice !== null && (
        <>
          <line
            x1={PAD_L}
            x2={W - PAD_R}
            y1={y(twap.config.limitPrice)}
            y2={y(twap.config.limitPrice)}
            stroke="var(--accent)"
            strokeWidth={1}
            strokeDasharray="4,3"
            opacity={0.7}
          />
          <text x={W - PAD_R + 4} y={y(twap.config.limitPrice) + 3} fill="var(--accent)" fontSize={10} fontFamily="ui-monospace, monospace">LIMIT {twap.config.limitPrice.toFixed(digits)}</text>
        </>
      )}

      {/* Mid price */}
      <path d={pricePath} stroke="#ececf1" strokeWidth={1.6} fill="none" />

      {/* Placed orders: BID green, OFFER red; skipped orders are hollow with dashed stroke */}
      {twap.orders.map((o: TwapOrder) => {
        const color = o.type === "BID" ? "var(--pos)" : "var(--neg)";
        return (
          <g key={o.seq}>
            <circle
              cx={x(o.t)}
              cy={y(o.price)}
              r={4.5}
              fill={o.skipped ? "var(--bg-card)" : color}
              stroke={color}
              strokeWidth={1.6}
              strokeDasharray={o.skipped ? "2,2" : undefined}
            />
          </g>
        );
      })}

      <circle cx={x(ticks[ticks.length - 1].t)} cy={y(lastPrice)} r={3.5} fill="#ececf1" />
    </svg>
  );
}
