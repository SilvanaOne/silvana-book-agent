"use client";

import type { Tick } from "@/lib/store";
import type { SpotGridState, SpotGridOrder } from "@/lib/spotgrid-engine";

type Props = Readonly<{ ticks: readonly Tick[]; spotgrid: SpotGridState | null }>;

const W = 720, H = 300, PAD_L = 60, PAD_R = 20, PAD_T = 14, PAD_B = 26;

export function SpotGridChart({ ticks, spotgrid }: Props) {
  if (ticks.length === 0 || !spotgrid) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start Spot Grid to see mid, grid rungs and fills.</div>;
  }
  const prices = ticks.map((t) => t.price);
  const rungPrices = spotgrid.orders.map((o) => o.price);

  const minAll = Math.min(...prices, ...rungPrices);
  const maxAll = Math.max(...prices, ...rungPrices);
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
  const openBids = spotgrid.orders.filter((o) => o.status === "open" && o.side === "BID").length;
  const openOffers = spotgrid.orders.filter((o) => o.status === "open" && o.side === "OFFER").length;
  const filled = spotgrid.orders.filter((o) => o.status === "filled").length;

  const rowY = PAD_T + 4;

  return (
    <svg viewBox={`0 0 ${W + 90} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 340 }}>
      <rect x={0} y={0} width={W + 90} height={H} fill="transparent" />
      {yTicks.map((v, i) => (
        <g key={i}>
          <line x1={PAD_L} x2={W - PAD_R} y1={y(v)} y2={y(v)} stroke="#22222c" strokeWidth={1} />
          <text x={PAD_L - 8} y={y(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{v.toFixed(digits)}</text>
        </g>
      ))}

      {/* Grid rungs: horizontal dashed lines for OPEN orders across the whole plot */}
      {spotgrid.orders.filter((o) => o.status === "open").map((o: SpotGridOrder) => {
        const color = o.side === "BID" ? "var(--pos)" : "var(--neg)";
        return (
          <g key={`open-${o.seq}`}>
            <line x1={PAD_L} x2={W - PAD_R} y1={y(o.price)} y2={y(o.price)} stroke={color} strokeWidth={1} strokeDasharray="4,4" opacity={0.55} />
            <text x={W - PAD_R + 4} y={y(o.price) + 3} fill={color} fontSize={10} fontFamily="ui-monospace, monospace" opacity={0.8}>{o.side === "BID" ? "B" : "O"}#{o.seq} {o.price.toFixed(digits)}</text>
          </g>
        );
      })}

      {/* Mid price line (on top of the grid) */}
      <path d={pricePath} stroke="#ececf1" strokeWidth={1.6} fill="none" />

      {/* Filled orders: solid disc at (filledAt, price) */}
      {spotgrid.orders.filter((o) => o.status === "filled" && o.filledAt !== undefined).map((o: SpotGridOrder) => {
        const color = o.side === "BID" ? "var(--pos)" : "var(--neg)";
        return (
          <circle key={`fill-${o.seq}`} cx={x(o.filledAt!)} cy={y(o.price)} r={5} fill={color} stroke={color} strokeWidth={1.4} />
        );
      })}

      {/* Current mid marker */}
      <circle cx={x(tMax)} cy={y(lastPrice)} r={3.5} fill="#ececf1" />

      {/* Legend */}
      <text x={PAD_L + 4} y={rowY + 8} fill="var(--text-faint)" fontSize={11} fontFamily="ui-monospace, monospace">
        open bids: <tspan fill="var(--pos)">{openBids}</tspan>  ·  open offers: <tspan fill="var(--neg)">{openOffers}</tspan>  ·  filled: <tspan fill="var(--accent)">{filled}</tspan>
      </text>
    </svg>
  );
}
