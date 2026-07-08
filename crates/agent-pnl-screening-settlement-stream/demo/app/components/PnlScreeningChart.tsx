"use client";

import type { Tick } from "@/lib/store";
import type { PnlScreeningState, PnlTrade } from "@/lib/pnlscreening-engine";

type Props = Readonly<{ ticks: readonly Tick[]; pnlscreening: PnlScreeningState | null }>;

const W = 720, H_TOP = 210, H_BOT = 150, GAP = 10, PAD_L = 60, PAD_R = 20, PAD_T = 14, PAD_B = 22;

export function PnlScreeningChart({ ticks, pnlscreening }: Props) {
  if (ticks.length === 0 || !pnlscreening) {
    return <div style={{ height: H_TOP + H_BOT + GAP, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start PnL Screening to see mid, trades and PnL.</div>;
  }

  const H = H_TOP + GAP + H_BOT;

  // -------- Top panel: price + trades --------
  const prices = ticks.map((t) => t.price);
  const tradePrices = pnlscreening.trades.map((tr) => tr.price);
  const priceMin = Math.min(...prices, ...tradePrices);
  const priceMax = Math.max(...prices, ...tradePrices);
  const pricePad = (priceMax - priceMin) * 0.1 || priceMin * 0.02;
  const pMin = priceMin - pricePad, pMax = priceMax + pricePad;
  const pRange = pMax - pMin || 1;

  const tMin = ticks[0].t, tMax = ticks[ticks.length - 1].t;
  const tRange = tMax - tMin || 1;
  const x = (t: number) => PAD_L + ((t - tMin) / tRange) * (W - PAD_L - PAD_R);
  const yPrice = (v: number) => PAD_T + (1 - (v - pMin) / pRange) * (H_TOP - PAD_T - PAD_B);

  const pricePath = ticks.map((t, i) => `${i === 0 ? "M" : "L"}${x(t.t).toFixed(1)},${yPrice(t.price).toFixed(1)}`).join(" ");

  const priceDigits = pMax > 100 ? 2 : pMax > 1 ? 4 : 6;
  const priceTicks: number[] = [];
  for (let i = 0; i <= 4; i++) priceTicks.push(pMin + (pRange * i) / 4);

  // Cost basis line (horizontal reference)
  const costBasis = pnlscreening.avgCostBasis;
  const showCost = pnlscreening.position > 0 && costBasis > 0 && costBasis >= pMin && costBasis <= pMax;

  // -------- Bottom panel: realized + unrealized PnL --------
  const realizedSeries = ticks.map((t) => t.realized);
  const unrealizedSeries = ticks.map((t) => t.unrealized);
  const pnlMin = Math.min(0, ...realizedSeries, ...unrealizedSeries);
  const pnlMax = Math.max(0, ...realizedSeries, ...unrealizedSeries);
  const pnlPad = (pnlMax - pnlMin) * 0.15 || 0.01;
  const plMin = pnlMin - pnlPad, plMax = pnlMax + pnlPad;
  const plRange = plMax - plMin || 1;

  const yBotTop = H_TOP + GAP + PAD_T;
  const yBotBottom = H_TOP + GAP + H_BOT - PAD_B;
  const yPnl = (v: number) => yBotTop + (1 - (v - plMin) / plRange) * (yBotBottom - yBotTop);

  const realizedColor = pnlscreening.realizedPnl >= 0 ? "var(--pos)" : "var(--neg)";
  const realizedPath = ticks.map((t, i) => `${i === 0 ? "M" : "L"}${x(t.t).toFixed(1)},${yPnl(t.realized).toFixed(1)}`).join(" ");
  const unrealizedPath = ticks.map((t, i) => `${i === 0 ? "M" : "L"}${x(t.t).toFixed(1)},${yPnl(t.unrealized).toFixed(1)}`).join(" ");

  const pnlDigits = Math.max(Math.abs(plMax), Math.abs(plMin)) > 100 ? 2 : 4;
  const pnlTicks: number[] = [];
  for (let i = 0; i <= 3; i++) pnlTicks.push(plMin + (plRange * i) / 3);

  const lastPrice = ticks[ticks.length - 1].price;

  // Triangle helpers for trades markers
  const triSize = (qty: number) => {
    const s = 3 + Math.min(4, Math.sqrt(Math.max(qty, 0.01)));
    return s;
  };

  return (
    <svg viewBox={`0 0 ${W + 90} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 420 }}>
      {/* ---- Top panel: price + trades ---- */}
      <rect x={0} y={0} width={W + 90} height={H_TOP} fill="transparent" />
      {priceTicks.map((v, i) => (
        <g key={`p${i}`}>
          <line x1={PAD_L} x2={W - PAD_R} y1={yPrice(v)} y2={yPrice(v)} stroke="#22222c" strokeWidth={1} />
          <text x={PAD_L - 8} y={yPrice(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{v.toFixed(priceDigits)}</text>
        </g>
      ))}

      {/* Cost basis reference line */}
      {showCost && (
        <g>
          <line x1={PAD_L} x2={W - PAD_R} y1={yPrice(costBasis)} y2={yPrice(costBasis)} stroke="var(--accent)" strokeDasharray="4,3" strokeWidth={1.2} opacity={0.85} />
          <text x={W - PAD_R + 4} y={yPrice(costBasis) + 3} fill="var(--accent)" fontSize={10} fontFamily="ui-monospace, monospace">cost {costBasis.toFixed(priceDigits)}</text>
        </g>
      )}

      {/* Mid price */}
      <path d={pricePath} stroke="#ececf1" strokeWidth={1.6} fill="none" />

      {/* Trade markers: triangle up (buy, green) / down (sell, red) */}
      {pnlscreening.trades.map((tr: PnlTrade) => {
        const cx = x(tr.t);
        const cy = yPrice(tr.price);
        const s = triSize(tr.qty);
        if (tr.side === "BUY") {
          return <polygon key={tr.seq} points={`${cx},${cy - s} ${cx - s},${cy + s} ${cx + s},${cy + s}`} fill="var(--pos)" stroke="var(--pos)" strokeWidth={1} opacity={0.9} />;
        } else {
          return <polygon key={tr.seq} points={`${cx},${cy + s} ${cx - s},${cy - s} ${cx + s},${cy - s}`} fill="var(--neg)" stroke="var(--neg)" strokeWidth={1} opacity={0.9} />;
        }
      })}

      <circle cx={x(tMax)} cy={yPrice(lastPrice)} r={3.5} fill="#ececf1" />

      {/* ---- Bottom panel: pnl ---- */}
      <rect x={0} y={H_TOP + GAP} width={W + 90} height={H_BOT} fill="transparent" />
      {pnlTicks.map((v, i) => (
        <g key={`pl${i}`}>
          <line x1={PAD_L} x2={W - PAD_R} y1={yPnl(v)} y2={yPnl(v)} stroke="#22222c" strokeWidth={1} />
          <text x={PAD_L - 8} y={yPnl(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{v.toFixed(pnlDigits)}</text>
        </g>
      ))}
      {/* Zero line */}
      {0 >= plMin && 0 <= plMax && (
        <line x1={PAD_L} x2={W - PAD_R} y1={yPnl(0)} y2={yPnl(0)} stroke="#3a3a48" strokeWidth={1.2} />
      )}

      {/* Realized pnl (color by sign of final) */}
      <path d={realizedPath} stroke={realizedColor} strokeWidth={1.6} fill="none" />
      {/* Unrealized pnl (dashed yellow) */}
      <path d={unrealizedPath} stroke="var(--accent)" strokeWidth={1.4} fill="none" strokeDasharray="4,3" opacity={0.9} />

      <text x={PAD_L + 4} y={H_TOP + GAP + PAD_T + 10} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">realized (solid) · unrealized (dashed)</text>
    </svg>
  );
}
