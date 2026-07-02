"use client";

import type { Tick } from "@/lib/store";
import type { WatchlistState } from "@/lib/watchlist-engine";

type Props = Readonly<{
  perMarketTicks: Record<string, readonly Tick[]>;
  watchlist: WatchlistState | null;
}>;

// Grid layout: 2 columns, N/2 rows of mini sparklines. Each sub-chart shows
// mid line + last price + % change vs start in the top-right corner.
const OUTER_W = 720;
const CELL_H = 130;
const COLS = 2;
const PAD_L = 46;
const PAD_R = 60;
const PAD_T = 22;
const PAD_B = 18;

export function WatchlistChart({ perMarketTicks, watchlist }: Props) {
  if (!watchlist || watchlist.snapshots.length === 0) {
    return (
      <div style={{ height: CELL_H * 2, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>
        No data — start Watchlist to see per-market mid sparklines.
      </div>
    );
  }

  const snaps = watchlist.snapshots;
  const rows = Math.ceil(snaps.length / COLS);
  const H = CELL_H * rows;
  const cellW = OUTER_W / COLS;

  return (
    <svg viewBox={`0 0 ${OUTER_W} ${H}`} style={{ width: "100%", height: "auto", maxHeight: CELL_H * rows + 20 }}>
      <rect x={0} y={0} width={OUTER_W} height={H} fill="transparent" />
      {snaps.map((snap, idx) => {
        const col = idx % COLS;
        const row = Math.floor(idx / COLS);
        const ox = col * cellW;
        const oy = row * CELL_H;
        const ticks = perMarketTicks[snap.market] ?? [];

        // Local plot area within the cell.
        const plotL = ox + PAD_L;
        const plotR = ox + cellW - PAD_R;
        const plotT = oy + PAD_T;
        const plotB = oy + CELL_H - PAD_B;

        let content: React.ReactNode = null;
        let priceLabel = fmt(snap.price);
        const changeCol = snap.priceChangeSinceStart >= 0 ? "var(--pos)" : "var(--neg)";
        const changeLabel = `${snap.priceChangeSinceStart >= 0 ? "+" : ""}${snap.priceChangeSinceStart.toFixed(2)}%`;

        if (ticks.length >= 2) {
          const prices = ticks.map((t) => t.price);
          const minP = Math.min(...prices);
          const maxP = Math.max(...prices);
          const pad = (maxP - minP) * 0.15 || minP * 0.005 || 0.01;
          const yMin = minP - pad, yMax = maxP + pad, yRange = yMax - yMin || 1;
          const tMin = ticks[0].t, tMax = ticks[ticks.length - 1].t;
          const tRange = tMax - tMin || 1;
          const x = (t: number) => plotL + ((t - tMin) / tRange) * (plotR - plotL);
          const y = (v: number) => plotT + (1 - (v - yMin) / yRange) * (plotB - plotT);

          const path = ticks.map((t, i) => `${i === 0 ? "M" : "L"}${x(t.t).toFixed(1)},${y(t.price).toFixed(1)}`).join(" ");
          const lastX = x(ticks[ticks.length - 1].t);
          const lastY = y(ticks[ticks.length - 1].price);
          content = (
            <>
              {/* baseline (starting price) */}
              <line x1={plotL} x2={plotR} y1={y(snap.startingPrice)} y2={y(snap.startingPrice)} stroke="#22222c" strokeWidth={1} strokeDasharray="3,3" />
              <path d={path} stroke={changeCol} strokeWidth={1.4} fill="none" opacity={0.95} />
              <circle cx={lastX} cy={lastY} r={2.8} fill="#ececf1" />
              {/* y-axis min/max ticks */}
              <text x={ox + PAD_L - 6} y={plotT + 4} fill="var(--text-faint)" fontSize={9} textAnchor="end" fontFamily="ui-monospace, monospace">{fmt(yMax)}</text>
              <text x={ox + PAD_L - 6} y={plotB} fill="var(--text-faint)" fontSize={9} textAnchor="end" fontFamily="ui-monospace, monospace">{fmt(yMin)}</text>
            </>
          );
        } else {
          content = (
            <text x={ox + cellW / 2} y={oy + CELL_H / 2} fill="var(--text-faint)" fontSize={10} textAnchor="middle" fontFamily="ui-monospace, monospace">warming up…</text>
          );
        }

        return (
          <g key={snap.market}>
            {/* cell border */}
            <rect x={ox + 2} y={oy + 2} width={cellW - 4} height={CELL_H - 4} fill="transparent" stroke="#22222c" strokeWidth={0.5} rx={4} />

            {/* market label top-left */}
            <text x={ox + 10} y={oy + 15} fill="var(--accent)" fontSize={11} fontFamily="ui-monospace, monospace" fontWeight={600}>{snap.market}</text>

            {/* price + change top-right */}
            <text x={ox + cellW - 10} y={oy + 15} fill="#ececf1" fontSize={11} textAnchor="end" fontFamily="ui-monospace, monospace">{priceLabel}</text>
            <text x={ox + cellW - 10} y={oy + 28} fill={changeCol} fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{changeLabel}</text>

            {content}
          </g>
        );
      })}
    </svg>
  );
}

function fmt(n: number): string {
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 6;
  return n.toFixed(digits);
}
