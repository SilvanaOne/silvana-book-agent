"use client";

import type { Tick } from "@/lib/store";
import type { OrderMatchingState, SnipeOrder } from "@/lib/ordermatching-engine";

type Props = Readonly<{ ticks: readonly Tick[]; ordermatching: OrderMatchingState | null }>;

const W = 720, H = 300, PAD_L = 60, PAD_R = 20, PAD_T = 14, PAD_B = 26;

export function OrderMatchingChart({ ticks, ordermatching }: Props) {
  if (ticks.length === 0 || !ordermatching) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start Order Matching to see mid, best bid/offer, triggers and snipes.</div>;
  }
  const prices = ticks.map((t) => t.price);
  const bids = ticks.map((t) => t.bestBid).filter((v): v is number => v !== null);
  const offers = ticks.map((t) => t.bestOffer).filter((v): v is number => v !== null);
  const orderPrices = ordermatching.orders.map((o) => o.price);
  const buyTrig = ordermatching.config.buyTrigger;
  const sellTrig = ordermatching.config.sellTrigger;
  const extras: number[] = [];
  if (buyTrig !== null) extras.push(buyTrig);
  if (sellTrig !== null) extras.push(sellTrig);

  const minAll = Math.min(...prices, ...bids, ...offers, ...orderPrices, ...extras);
  const maxAll = Math.max(...prices, ...bids, ...offers, ...orderPrices, ...extras);
  const pad = (maxAll - minAll) * 0.1 || minAll * 0.02;
  const yMin = minAll - pad, yMax = maxAll + pad, yRange = yMax - yMin || 1;
  const tMin = ticks[0].t, tMax = ticks[ticks.length - 1].t;
  const tRange = tMax - tMin || 1;
  const x = (t: number) => PAD_L + ((t - tMin) / tRange) * (W - PAD_L - PAD_R);
  const y = (v: number) => PAD_T + (1 - (v - yMin) / yRange) * (H - PAD_T - PAD_B);

  const pricePath = ticks.map((t, i) => `${i === 0 ? "M" : "L"}${x(t.t).toFixed(1)},${y(t.price).toFixed(1)}`).join(" ");
  const bidPath = ticks
    .map((t, i) => t.bestBid === null ? null : `${i === 0 ? "M" : "L"}${x(t.t).toFixed(1)},${y(t.bestBid).toFixed(1)}`)
    .filter((s): s is string => s !== null)
    .join(" ");
  const offerPath = ticks
    .map((t, i) => t.bestOffer === null ? null : `${i === 0 ? "M" : "L"}${x(t.t).toFixed(1)},${y(t.bestOffer).toFixed(1)}`)
    .filter((s): s is string => s !== null)
    .join(" ");

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

      {/* Trigger lines (horizontal, dashed) */}
      {buyTrig !== null && (
        <g>
          <line x1={PAD_L} x2={W - PAD_R} y1={y(buyTrig)} y2={y(buyTrig)} stroke="var(--pos)" strokeWidth={1.2} strokeDasharray="5,4" opacity={0.85} />
          <text x={W - PAD_R + 4} y={y(buyTrig) + 3} fill="var(--pos)" fontSize={10} fontFamily="ui-monospace, monospace">buy≤{buyTrig.toFixed(digits)}</text>
        </g>
      )}
      {sellTrig !== null && (
        <g>
          <line x1={PAD_L} x2={W - PAD_R} y1={y(sellTrig)} y2={y(sellTrig)} stroke="var(--neg)" strokeWidth={1.2} strokeDasharray="5,4" opacity={0.85} />
          <text x={W - PAD_R + 4} y={y(sellTrig) + 3} fill="var(--neg)" fontSize={10} fontFamily="ui-monospace, monospace">sell≥{sellTrig.toFixed(digits)}</text>
        </g>
      )}

      {/* Best bid (green, below) */}
      {bidPath && <path d={bidPath} stroke="var(--pos)" strokeWidth={1.2} fill="none" opacity={0.55} />}
      {/* Best offer (red, above) */}
      {offerPath && <path d={offerPath} stroke="var(--neg)" strokeWidth={1.2} fill="none" opacity={0.55} />}

      {/* Mid price (white) */}
      <path d={pricePath} stroke="#ececf1" strokeWidth={1.6} fill="none" />

      {/* Snipes: hollow circle = open, filled disc = filled */}
      {ordermatching.orders.map((o: SnipeOrder) => {
        const color = o.side === "BID" ? "var(--pos)" : "var(--neg)";
        const fillCol = o.status === "filled" ? color : "var(--bg-card)";
        return (
          <g key={o.seq}>
            <circle cx={x(o.t)} cy={y(o.price)} r={4.5} fill={fillCol} stroke={color} strokeWidth={1.6} />
          </g>
        );
      })}

      <circle cx={x(tMax)} cy={y(lastPrice)} r={3.5} fill="#ececf1" />
    </svg>
  );
}
