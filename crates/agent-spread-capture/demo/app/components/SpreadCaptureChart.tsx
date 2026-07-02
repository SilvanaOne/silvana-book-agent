"use client";

import type { Tick } from "@/lib/store";
import type { SpreadCaptureState, SpreadCaptureOrder } from "@/lib/spreadcapture-engine";

type Props = Readonly<{ ticks: readonly Tick[]; spreadcapture: SpreadCaptureState | null }>;

const W = 720, H = 300, PAD_L = 60, PAD_R = 20, PAD_T = 14, PAD_B = 26;

export function SpreadCaptureChart({ ticks, spreadcapture }: Props) {
  if (ticks.length === 0 || !spreadcapture) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start Spread Capture to see mid, active bid/offer, fills and inventory.</div>;
  }
  const prices = ticks.map((t) => t.price);
  const orderPrices = spreadcapture.orders.map((o) => o.price);
  const quotePrices: number[] = [];
  if (spreadcapture.bidActive) quotePrices.push(spreadcapture.bidActive.price);
  if (spreadcapture.offerActive) quotePrices.push(spreadcapture.offerActive.price);

  const minAll = Math.min(...prices, ...orderPrices, ...quotePrices);
  const maxAll = Math.max(...prices, ...orderPrices, ...quotePrices);
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

  // Inventory bar (bottom-right of the plot area).
  const maxInv = spreadcapture.config.maxInventory || 1;
  const invClamp = Math.max(-maxInv, Math.min(maxInv, spreadcapture.netInventory));
  const barW = 120, barH = 8;
  const barX = W - PAD_R - barW;
  const barY = H - PAD_B - barH - 6;
  const invFrac = invClamp / maxInv; // -1..1
  const invColor = invFrac > 0 ? "var(--pos)" : invFrac < 0 ? "var(--neg)" : "var(--text-faint)";
  const fillW = Math.abs(invFrac) * (barW / 2);
  const fillX = invFrac >= 0 ? barX + barW / 2 : barX + barW / 2 - fillW;

  return (
    <svg viewBox={`0 0 ${W + 90} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 340 }}>
      <rect x={0} y={0} width={W + 90} height={H} fill="transparent" />
      {yTicks.map((v, i) => (
        <g key={i}>
          <line x1={PAD_L} x2={W - PAD_R} y1={y(v)} y2={y(v)} stroke="#22222c" strokeWidth={1} />
          <text x={PAD_L - 8} y={y(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{v.toFixed(digits)}</text>
        </g>
      ))}

      {/* Active bid/offer as solid horizontal lines. */}
      {spreadcapture.bidActive && (
        <line
          x1={PAD_L}
          x2={W - PAD_R}
          y1={y(spreadcapture.bidActive.price)}
          y2={y(spreadcapture.bidActive.price)}
          stroke="var(--pos)"
          strokeWidth={1.4}
          opacity={0.85}
        />
      )}
      {spreadcapture.offerActive && (
        <line
          x1={PAD_L}
          x2={W - PAD_R}
          y1={y(spreadcapture.offerActive.price)}
          y2={y(spreadcapture.offerActive.price)}
          stroke="var(--neg)"
          strokeWidth={1.4}
          opacity={0.85}
        />
      )}

      {/* Mid price. */}
      <path d={pricePath} stroke="#ececf1" strokeWidth={1.6} fill="none" />

      {/* Fills: filled disc, colored per side. */}
      {spreadcapture.orders
        .filter((o: SpreadCaptureOrder) => o.status === "filled" && o.filledAt !== undefined)
        .map((o: SpreadCaptureOrder) => {
          const color = o.side === "BID" ? "var(--pos)" : "var(--neg)";
          return (
            <g key={`f-${o.seq}`}>
              <circle cx={x(o.filledAt as number)} cy={y(o.price)} r={4.5} fill={color} stroke={color} strokeWidth={1.4} />
            </g>
          );
        })}

      <circle cx={x(tMax)} cy={y(lastPrice)} r={3.5} fill="#ececf1" />

      {/* Right-margin labels for the active quotes. */}
      {spreadcapture.bidActive && (
        <text x={W - PAD_R + 4} y={y(spreadcapture.bidActive.price) + 3} fill="var(--pos)" fontSize={10} fontFamily="ui-monospace, monospace">BID {spreadcapture.bidActive.price.toFixed(digits)}</text>
      )}
      {spreadcapture.offerActive && (
        <text x={W - PAD_R + 4} y={y(spreadcapture.offerActive.price) + 3} fill="var(--neg)" fontSize={10} fontFamily="ui-monospace, monospace">OFFER {spreadcapture.offerActive.price.toFixed(digits)}</text>
      )}

      {/* Inventory bar. */}
      <g>
        <rect x={barX} y={barY} width={barW} height={barH} fill="#1a1a22" stroke="#22222c" />
        <line x1={barX + barW / 2} x2={barX + barW / 2} y1={barY - 2} y2={barY + barH + 2} stroke="#3f3f4a" strokeWidth={1} />
        <rect x={fillX} y={barY} width={fillW} height={barH} fill={invColor} opacity={0.85} />
        <text x={barX} y={barY - 4} fill="var(--text-faint)" fontSize={9} fontFamily="ui-monospace, monospace">−{maxInv}</text>
        <text x={barX + barW} y={barY - 4} fill="var(--text-faint)" fontSize={9} textAnchor="end" fontFamily="ui-monospace, monospace">+{maxInv}</text>
        <text x={barX + barW / 2} y={barY + barH + 12} fill="var(--text-faint)" fontSize={9} textAnchor="middle" fontFamily="ui-monospace, monospace">
          inv {spreadcapture.netInventory >= 0 ? "+" : ""}{spreadcapture.netInventory.toFixed(3)}
        </text>
      </g>
    </svg>
  );
}
