"use client";

import type { LiquiditySeekingState, Level } from "@/lib/liquidityseeking-engine";

type Props = Readonly<{ liquidityseeking: LiquiditySeekingState | null }>;

const W = 780, H = 320, PAD_L = 80, PAD_R = 24, PAD_T = 16, PAD_B = 34;

export function LiquiditySeekingChart({ liquidityseeking }: Props) {
  if (!liquidityseeking) {
    return (
      <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>
        No data — start Liquidity Seeking to see the book, the active child order and completed fills.
      </div>
    );
  }

  const { book, currentPrice, currentChild, children, config } = liquidityseeking;
  const bids = book.bids;
  const offers = book.offers;

  // Y (price) range covering both sides, live child, filled children.
  const allPx: number[] = [];
  for (const l of bids) allPx.push(l.price);
  for (const l of offers) allPx.push(l.price);
  allPx.push(currentPrice);
  for (const c of children) allPx.push(c.price);
  const minPx = Math.min(...allPx);
  const maxPx = Math.max(...allPx);
  const pad = (maxPx - minPx) * 0.15 || minPx * 0.02;
  const yMin = minPx - pad, yMax = maxPx + pad, yRange = yMax - yMin || 1;

  // X: depth qty. Symmetric around 0 — bids left, offers right.
  const maxQty = Math.max(1, ...bids.map((l) => l.qty), ...offers.map((l) => l.qty));
  const midX = (W - PAD_L - PAD_R) / 2 + PAD_L;
  const halfW = (W - PAD_L - PAD_R) / 2;
  const rowH = (H - PAD_T - PAD_B) / Math.max(config.depth, 1);

  const y = (px: number) => PAD_T + (1 - (px - yMin) / yRange) * (H - PAD_T - PAD_B);

  const digits = yMax > 100 ? 2 : yMax > 1 ? 4 : 6;
  const yTicks: number[] = [];
  for (let i = 0; i <= 4; i++) yTicks.push(yMin + (yRange * i) / 4);

  return (
    <svg viewBox={`0 0 ${W + 60} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 360 }}>
      <rect x={0} y={0} width={W + 60} height={H} fill="transparent" />
      {yTicks.map((v, i) => (
        <g key={i}>
          <line x1={PAD_L} x2={W - PAD_R} y1={y(v)} y2={y(v)} stroke="#22222c" strokeWidth={1} />
          <text x={PAD_L - 8} y={y(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{v.toFixed(digits)}</text>
        </g>
      ))}

      {/* Vertical mid axis */}
      <line x1={midX} x2={midX} y1={PAD_T} y2={H - PAD_B} stroke="#33333f" strokeWidth={1} />

      {/* Bids (left) */}
      {bids.map((l: Level, i) => {
        const w = (l.qty / maxQty) * halfW;
        return (
          <rect
            key={`b${i}`}
            x={midX - w}
            y={y(l.price) - rowH * 0.35}
            width={w}
            height={rowH * 0.7}
            fill="var(--pos)"
            opacity={0.32}
          />
        );
      })}

      {/* Offers (right) */}
      {offers.map((l: Level, i) => {
        const w = (l.qty / maxQty) * halfW;
        return (
          <rect
            key={`o${i}`}
            x={midX}
            y={y(l.price) - rowH * 0.35}
            width={w}
            height={rowH * 0.7}
            fill="var(--neg)"
            opacity={0.32}
          />
        );
      })}

      {/* Mid line (horizontal) */}
      <line x1={PAD_L} x2={W - PAD_R} y1={y(currentPrice)} y2={y(currentPrice)} stroke="#ececf1" strokeWidth={1.4} strokeDasharray="4,3" opacity={0.7} />
      <text x={W - PAD_R + 4} y={y(currentPrice) + 3} fill="#ececf1" fontSize={10} fontFamily="ui-monospace, monospace">MID {currentPrice.toFixed(digits)}</text>

      {/* Completed children — small green disc offset by time within window. */}
      {children.filter((c) => c.status === "filled").map((c) => {
        // Position on x based on child seq (evenly spread on side band). This
        // is illustrative — the primary chart axis is price / depth.
        const xOffset = c.side === "BID" ? -halfW * 0.9 + ((c.seq * 11) % Math.max(1, halfW * 0.7))
                                          : halfW * 0.2 + ((c.seq * 11) % Math.max(1, halfW * 0.7));
        return (
          <circle key={`f${c.seq}`} cx={midX + xOffset} cy={y(c.filledPrice ?? c.price)} r={3.6} fill="var(--pos)" opacity={0.9} />
        );
      })}

      {/* Active child — thick outlined rect on the correct side. */}
      {currentChild && currentChild.status === "open" && (() => {
        const c = currentChild;
        const barW = Math.max(6, (c.qty / maxQty) * halfW);
        const barX = c.side === "BID" ? midX - barW : midX;
        const color = c.side === "BID" ? "var(--pos)" : "var(--neg)";
        return (
          <g>
            <rect x={barX} y={y(c.price) - rowH * 0.5} width={barW} height={rowH} fill="none" stroke={color} strokeWidth={2.4} />
            <text x={W - PAD_R + 4} y={y(c.price) + 3} fill={color} fontSize={10} fontFamily="ui-monospace, monospace">
              #{c.seq} {c.side} {c.qty.toFixed(4)} · {c.slippageBps.toFixed(1)}bps
            </text>
          </g>
        );
      })()}

      {/* Legend / axis labels */}
      <text x={midX - halfW / 2} y={H - PAD_B + 18} fill="var(--pos)" fontSize={10} textAnchor="middle" fontFamily="ui-monospace, monospace">BIDS · depth</text>
      <text x={midX + halfW / 2} y={H - PAD_B + 18} fill="var(--neg)" fontSize={10} textAnchor="middle" fontFamily="ui-monospace, monospace">OFFERS · depth</text>
    </svg>
  );
}
