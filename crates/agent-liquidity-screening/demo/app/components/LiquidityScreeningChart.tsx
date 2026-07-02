"use client";

import type { Tick } from "@/lib/store";
import type { LiquidityScreeningState } from "@/lib/liquidityscreening-engine";

type Props = Readonly<{ ticks: readonly Tick[]; liquidityscreening: LiquidityScreeningState | null }>;

const W = 720, H = 320, PAD_L = 70, PAD_R = 100, PAD_T = 14, PAD_B = 36;

export function LiquidityScreeningChart({ ticks, liquidityscreening }: Props) {
  void ticks;
  if (!liquidityscreening || liquidityscreening.bidLevels.length === 0) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start Liquidity to see book depth, spread and slippage probes.</div>;
  }
  const bids = liquidityscreening.bidLevels;
  const offers = liquidityscreening.offerLevels;
  const mid = liquidityscreening.currentPrice;

  const maxQty = Math.max(
    ...bids.map((l) => l.qty),
    ...offers.map((l) => l.qty),
    1e-9,
  );
  const allPrices = [...bids.map((l) => l.price), ...offers.map((l) => l.price), mid];
  const pMin = Math.min(...allPrices);
  const pMax = Math.max(...allPrices);
  const pRange = pMax - pMin || 1;

  // X-axis: qty on both sides. Center of chart = price=0 qty axis. Bids extend LEFT, offers RIGHT.
  const cx = PAD_L + (W - PAD_L - PAD_R) / 2;
  const halfWidth = (W - PAD_L - PAD_R) / 2;
  const qtyScale = (q: number) => (q / maxQty) * halfWidth;

  const y = (p: number) => PAD_T + (1 - (p - pMin) / pRange) * (H - PAD_T - PAD_B);
  const rowH = Math.max(4, (H - PAD_T - PAD_B) / Math.max(bids.length, offers.length) / 1.4);

  const digits = pMax > 100 ? 2 : pMax > 1 ? 4 : 6;

  const yTicks: number[] = [];
  for (let i = 0; i <= 5; i++) yTicks.push(pMin + (pRange * i) / 5);

  const buyVwap = liquidityscreening.buyProbeVwap;
  const sellVwap = liquidityscreening.sellProbeVwap;

  return (
    <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 360 }}>
      {/* Y-axis grid + price labels */}
      {yTicks.map((v, i) => (
        <g key={i}>
          <line x1={PAD_L} x2={W - PAD_R} y1={y(v)} y2={y(v)} stroke="#22222c" strokeWidth={1} />
          <text x={PAD_L - 8} y={y(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{v.toFixed(digits)}</text>
        </g>
      ))}

      {/* Center qty axis */}
      <line x1={cx} x2={cx} y1={PAD_T} y2={H - PAD_B} stroke="#3a3a4a" strokeWidth={1} />

      {/* Bid bars (green, left of center) */}
      {bids.map((l, i) => {
        const w = qtyScale(l.qty);
        const yy = y(l.price) - rowH / 2;
        return (
          <g key={`b${i}`}>
            <rect x={cx - w} y={yy} width={w} height={rowH} fill="var(--pos)" opacity={0.55} />
            <text x={cx - w - 3} y={yy + rowH / 2 + 3} fill="var(--pos)" fontSize={9} textAnchor="end" fontFamily="ui-monospace, monospace">{l.qty.toFixed(2)}</text>
          </g>
        );
      })}

      {/* Offer bars (red, right of center) */}
      {offers.map((l, i) => {
        const w = qtyScale(l.qty);
        const yy = y(l.price) - rowH / 2;
        return (
          <g key={`o${i}`}>
            <rect x={cx} y={yy} width={w} height={rowH} fill="var(--neg)" opacity={0.55} />
            <text x={cx + w + 3} y={yy + rowH / 2 + 3} fill="var(--neg)" fontSize={9} fontFamily="ui-monospace, monospace">{l.qty.toFixed(2)}</text>
          </g>
        );
      })}

      {/* Mid line */}
      <line x1={PAD_L} x2={W - PAD_R} y1={y(mid)} y2={y(mid)} stroke="#ececf1" strokeWidth={1.4} strokeDasharray="4,3" />
      <text x={W - PAD_R + 4} y={y(mid) + 3} fill="#ececf1" fontSize={10} fontFamily="ui-monospace, monospace">mid {mid.toFixed(digits)}</text>

      {/* Buy probe VWAP marker (above mid, offer side) */}
      {buyVwap !== null && (
        <>
          <line x1={cx} x2={W - PAD_R} y1={y(buyVwap)} y2={y(buyVwap)} stroke="var(--neg)" strokeWidth={1} strokeDasharray="2,3" />
          <text x={W - PAD_R + 4} y={y(buyVwap) + 3} fill="var(--neg)" fontSize={10} fontFamily="ui-monospace, monospace">buy vwap {buyVwap.toFixed(digits)}</text>
        </>
      )}
      {/* Sell probe VWAP marker (below mid, bid side) */}
      {sellVwap !== null && (
        <>
          <line x1={PAD_L} x2={cx} y1={y(sellVwap)} y2={y(sellVwap)} stroke="var(--pos)" strokeWidth={1} strokeDasharray="2,3" />
          <text x={PAD_L + 4} y={y(sellVwap) - 2} fill="var(--pos)" fontSize={10} fontFamily="ui-monospace, monospace">sell vwap {sellVwap.toFixed(digits)}</text>
        </>
      )}

      {/* X-axis labels */}
      <text x={cx - halfWidth} y={H - PAD_B + 14} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">{maxQty.toFixed(2)}</text>
      <text x={cx} y={H - PAD_B + 14} fill="var(--text-faint)" fontSize={10} textAnchor="middle" fontFamily="ui-monospace, monospace">qty</text>
      <text x={cx + halfWidth} y={H - PAD_B + 14} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{maxQty.toFixed(2)}</text>
      <text x={cx - halfWidth / 2} y={H - 6} fill="var(--pos)" fontSize={10} textAnchor="middle" fontFamily="ui-monospace, monospace">BIDS</text>
      <text x={cx + halfWidth / 2} y={H - 6} fill="var(--neg)" fontSize={10} textAnchor="middle" fontFamily="ui-monospace, monospace">OFFERS</text>
    </svg>
  );
}
