"use client";

import type { Tick } from "@/lib/store";
import type { DcaState, DcaOrder } from "@/lib/dca-engine";

type Props = Readonly<{ ticks: readonly Tick[]; dca: DcaState | null }>;

const W = 720, H = 300, PAD_L = 60, PAD_R = 20, PAD_T = 14, PAD_B = 26;

export function DcaChart({ ticks, dca }: Props) {
  if (ticks.length === 0 || !dca) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start DCA to see live price + orders.</div>;
  }
  const prices = ticks.map((t) => t.price);
  const orderPrices = dca.orders.map((o) => o.price);
  const extra: number[] = [];
  if (dca.avgPrice > 0) extra.push(dca.avgPrice);

  const minAll = Math.min(...prices, ...orderPrices, ...extra);
  const maxAll = Math.max(...prices, ...orderPrices, ...extra);
  const pad = (maxAll - minAll) * 0.1 || minAll * 0.02;
  const yMin = minAll - pad, yMax = maxAll + pad, yRange = yMax - yMin || 1;
  const tMin = ticks[0].t, tMax = ticks[ticks.length - 1].t;
  const tRange = tMax - tMin || 1;
  const x = (t: number) => PAD_L + ((t - tMin) / tRange) * (W - PAD_L - PAD_R);
  const y = (v: number) => PAD_T + (1 - (v - yMin) / yRange) * (H - PAD_T - PAD_B);

  const pathD = ticks.map((t, i) => `${i === 0 ? "M" : "L"}${x(t.t).toFixed(1)},${y(t.price).toFixed(1)}`).join(" ");
  const digits = yMax > 100 ? 2 : yMax > 1 ? 4 : 6;
  const yTicks: number[] = [];
  for (let i = 0; i <= 4; i++) yTicks.push(yMin + (yRange * i) / 4);

  const orderColor = dca.config.side === "buy" ? "var(--pos)" : "var(--neg)";

  return (
    <svg viewBox={`0 0 ${W + 90} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 340 }}>
      <rect x={0} y={0} width={W + 90} height={H} fill="transparent" />
      {yTicks.map((v, i) => (
        <g key={i}>
          <line x1={PAD_L} x2={W - PAD_R} y1={y(v)} y2={y(v)} stroke="#22222c" strokeWidth={1} />
          <text x={PAD_L - 8} y={y(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{v.toFixed(digits)}</text>
        </g>
      ))}

      {dca.avgPrice > 0 && (
        <>
          <line x1={PAD_L} x2={W - PAD_R} y1={y(dca.avgPrice)} y2={y(dca.avgPrice)} stroke="var(--accent)" strokeWidth={1} strokeDasharray="4,4" opacity={0.9} />
          <text x={W - PAD_R + 4} y={y(dca.avgPrice) + 3} fill="var(--accent)" fontSize={10} fontFamily="ui-monospace, monospace">avg {dca.avgPrice.toFixed(digits)}</text>
        </>
      )}

      <path d={pathD} stroke="#ffa347" strokeWidth={1.8} fill="none" />
      <path d={`${pathD} L${x(tMax).toFixed(1)},${(H - PAD_B).toFixed(1)} L${x(tMin).toFixed(1)},${(H - PAD_B).toFixed(1)} Z`} fill="url(#ograd)" opacity={0.3} />
      <defs>
        <linearGradient id="ograd" x1="0" x2="0" y1="0" y2="1">
          <stop offset="0%" stopColor="#ff8a1c" stopOpacity="0.5" />
          <stop offset="100%" stopColor="#ff8a1c" stopOpacity="0" />
        </linearGradient>
      </defs>

      {dca.orders.map((o: DcaOrder) => (
        <g key={o.seq}>
          <circle cx={x(o.t)} cy={y(o.price)} r={4} fill={orderColor} stroke="var(--bg-card)" strokeWidth={1.5} />
        </g>
      ))}

      <circle cx={x(tMax)} cy={y(ticks[ticks.length - 1].price)} r={3.5} fill="#ff8a1c" />
    </svg>
  );
}
