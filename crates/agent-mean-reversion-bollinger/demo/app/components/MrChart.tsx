"use client";

import type { Tick } from "@/lib/store";
import type { MrState, MrOrder } from "@/lib/mr-engine";

type Props = Readonly<{ ticks: readonly Tick[]; mr: MrState | null }>;

const W = 720, H = 300, PAD_L = 60, PAD_R = 20, PAD_T = 14, PAD_B = 26;

export function MrChart({ ticks, mr }: Props) {
  if (ticks.length === 0 || !mr) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start Mean Reversion to see mid, SMA, Bollinger bands and signals.</div>;
  }
  const prices = ticks.map((t) => t.price);
  const smas = ticks.map((t) => t.sma).filter((v): v is number => v !== null);
  const uppers = ticks.map((t) => t.upper).filter((v): v is number => v !== null);
  const lowers = ticks.map((t) => t.lower).filter((v): v is number => v !== null);
  const orderPrices = mr.orders.map((o) => o.price);

  const allValues = [...prices, ...smas, ...uppers, ...lowers, ...orderPrices];
  const minAll = Math.min(...allValues);
  const maxAll = Math.max(...allValues);
  const pad = (maxAll - minAll) * 0.1 || (minAll || 1) * 0.02;
  const yMin = minAll - pad, yMax = maxAll + pad, yRange = yMax - yMin || 1;
  const tMin = ticks[0].t, tMax = ticks[ticks.length - 1].t;
  const tRange = tMax - tMin || 1;
  const x = (t: number) => PAD_L + ((t - tMin) / tRange) * (W - PAD_L - PAD_R);
  const y = (v: number) => PAD_T + (1 - (v - yMin) / yRange) * (H - PAD_T - PAD_B);

  const pricePath = ticks.map((t, i) => `${i === 0 ? "M" : "L"}${x(t.t).toFixed(1)},${y(t.price).toFixed(1)}`).join(" ");

  function seriesPath(getter: (t: Tick) => number | null): string {
    let started = false;
    const parts: string[] = [];
    for (const t of ticks) {
      const v = getter(t);
      if (v === null) { started = false; continue; }
      parts.push(`${started ? "L" : "M"}${x(t.t).toFixed(1)},${y(v).toFixed(1)}`);
      started = true;
    }
    return parts.join(" ");
  }
  const smaPath = seriesPath((t) => t.sma);
  const upperPath = seriesPath((t) => t.upper);
  const lowerPath = seriesPath((t) => t.lower);

  const digits = yMax > 100 ? 2 : yMax > 1 ? 4 : 6;
  const yTicks: number[] = [];
  for (let i = 0; i <= 4; i++) yTicks.push(yMin + (yRange * i) / 4);

  const lastPrice = ticks[ticks.length - 1].price;

  return (
    <svg viewBox={`0 0 ${W + 90} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 340 }}>
      <rect x={0} y={0} width={W + 90} height={H} fill="transparent" />
      {yTicks.map((v, i) => (
        <g key={i}>
          <line x1={PAD_L} x2={W - PAD_R} y1={y(v)} y2={y(v)} stroke="#22222c" strokeWidth={1} />
          <text x={PAD_L - 8} y={y(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{v.toFixed(digits)}</text>
        </g>
      ))}

      {/* Bollinger bands (upper/lower) */}
      {upperPath && <path d={upperPath} stroke="#3f5b8a" strokeWidth={1} strokeDasharray="3,3" fill="none" opacity={0.75} />}
      {lowerPath && <path d={lowerPath} stroke="#3f5b8a" strokeWidth={1} strokeDasharray="3,3" fill="none" opacity={0.75} />}

      {/* SMA line */}
      {smaPath && <path d={smaPath} stroke="var(--accent)" strokeWidth={1.6} fill="none" opacity={0.9} />}

      {/* Mid price */}
      <path d={pricePath} stroke="#ececf1" strokeWidth={1.6} fill="none" />

      {/* Orders: circle for open, filled disc for filled */}
      {mr.orders.map((o: MrOrder) => {
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
      {mr.sma !== null && (
        <text x={W - PAD_R + 4} y={y(mr.sma) + 3} fill="var(--accent)" fontSize={10} fontFamily="ui-monospace, monospace">SMA {mr.sma.toFixed(digits)}</text>
      )}
      {mr.upper !== null && (
        <text x={W - PAD_R + 4} y={y(mr.upper) + 3} fill="#7a95c4" fontSize={10} fontFamily="ui-monospace, monospace">U {mr.upper.toFixed(digits)}</text>
      )}
      {mr.lower !== null && (
        <text x={W - PAD_R + 4} y={y(mr.lower) + 3} fill="#7a95c4" fontSize={10} fontFamily="ui-monospace, monospace">L {mr.lower.toFixed(digits)}</text>
      )}
    </svg>
  );
}
