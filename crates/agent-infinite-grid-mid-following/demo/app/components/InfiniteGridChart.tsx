"use client";

import type { Tick } from "@/lib/store";
import type { InfiniteGridState, InfiniteGridOrder } from "@/lib/infinitegrid-engine";

type Props = Readonly<{ ticks: readonly Tick[]; infinitegrid: InfiniteGridState | null }>;

const W = 720, H = 300, PAD_L = 60, PAD_R = 20, PAD_T = 14, PAD_B = 26;

export function InfiniteGridChart({ ticks, infinitegrid }: Props) {
  if (ticks.length === 0 || !infinitegrid) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start Infinite Grid to see mid, ladder and fills.</div>;
  }
  const prices = ticks.map((t) => t.price);
  const active = infinitegrid.activeOrders;
  const fills = infinitegrid.history.filter((o) => o.status === "filled" && o.filledAt !== undefined);
  const rebuildTs = infinitegrid.history
    .filter((o) => o.status === "cancelled" && o.cancelledAt !== undefined)
    .map((o) => o.cancelledAt as number);
  const uniqRebuildTs = Array.from(new Set(rebuildTs));

  const laddersPrices = active.map((o) => o.price);
  const fillPrices = fills.map((o) => o.price);

  const center = infinitegrid.gridCenter ?? undefined;

  const values: number[] = [...prices, ...laddersPrices, ...fillPrices];
  if (center !== undefined) values.push(center);
  const minAll = Math.min(...values);
  const maxAll = Math.max(...values);
  const pad = (maxAll - minAll) * 0.1 || (minAll || 1) * 0.02;
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

  return (
    <svg viewBox={`0 0 ${W + 90} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 340 }}>
      <rect x={0} y={0} width={W + 90} height={H} fill="transparent" />
      {yTicks.map((v, i) => (
        <g key={i}>
          <line x1={PAD_L} x2={W - PAD_R} y1={y(v)} y2={y(v)} stroke="#22222c" strokeWidth={1} />
          <text x={PAD_L - 8} y={y(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{v.toFixed(digits)}</text>
        </g>
      ))}

      {/* Rebuild markers: vertical dashed lines */}
      {uniqRebuildTs.map((t, i) => (
        <line key={`rb-${i}`} x1={x(t)} x2={x(t)} y1={PAD_T} y2={H - PAD_B} stroke="var(--accent)" strokeWidth={1} strokeDasharray="2,3" opacity={0.35} />
      ))}

      {/* Grid center — solid orange horizontal */}
      {center !== undefined && (
        <line x1={PAD_L} x2={W - PAD_R} y1={y(center)} y2={y(center)} stroke="var(--accent)" strokeWidth={1.2} opacity={0.85} />
      )}

      {/* Active ladder — dashed horizontal levels */}
      {active.map((o: InfiniteGridOrder) => {
        const color = o.side === "BID" ? "var(--pos)" : "var(--neg)";
        return (
          <line
            key={`lv-${o.seq}`}
            x1={x(Math.max(tMin, o.t))}
            x2={W - PAD_R}
            y1={y(o.price)}
            y2={y(o.price)}
            stroke={color}
            strokeWidth={1}
            strokeDasharray="4,3"
            opacity={0.75}
          />
        );
      })}

      {/* Fill dots */}
      {fills.map((o: InfiniteGridOrder) => {
        const color = o.side === "BID" ? "var(--pos)" : "var(--neg)";
        return (
          <circle
            key={`fl-${o.seq}`}
            cx={x(o.filledAt as number)}
            cy={y(o.price)}
            r={4}
            fill={color}
            stroke={color}
            strokeWidth={1.2}
          />
        );
      })}

      {/* Mid price */}
      <path d={pricePath} stroke="#ececf1" strokeWidth={1.6} fill="none" />
      <circle cx={x(tMax)} cy={y(lastPrice)} r={3.5} fill="#ececf1" />

      {/* Right-margin labels */}
      {center !== undefined && (
        <text x={W - PAD_R + 4} y={y(center) + 3} fill="var(--accent)" fontSize={10} fontFamily="ui-monospace, monospace">CENTER {center.toFixed(digits)}</text>
      )}
    </svg>
  );
}
