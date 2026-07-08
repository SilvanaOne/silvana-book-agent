"use client";

import type { Tick } from "@/lib/store";
import type { HedgingState, HedgeOrder } from "@/lib/hedging-engine";

type Props = Readonly<{ ticks: readonly Tick[]; hedging: HedgingState | null }>;

const W = 720;
const H_TOP = 180;
const H_BOT = 150;
const GAP = 18;
const H = H_TOP + GAP + H_BOT;
const PAD_L = 60;
const PAD_R = 20;
const PAD_T = 14;
const PAD_B = 26;

export function HedgingChart({ ticks, hedging }: Props) {
  if (ticks.length === 0 || !hedging) {
    return (
      <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>
        No data — start Hedging to see price, hedge fills, balance and target band.
      </div>
    );
  }

  const tMin = ticks[0].t;
  const tMax = ticks[ticks.length - 1].t;
  const tRange = tMax - tMin || 1;
  const x = (t: number) => PAD_L + ((t - tMin) / tRange) * (W - PAD_L - PAD_R);

  // ---- Top pane: mid price + hedge fills ----
  const prices = ticks.map((t) => t.price);
  const orderPrices = hedging.orders.map((o) => o.price);
  const pMinAll = Math.min(...prices, ...orderPrices);
  const pMaxAll = Math.max(...prices, ...orderPrices);
  const pPad = (pMaxAll - pMinAll) * 0.1 || pMinAll * 0.02 || 0.001;
  const pMin = pMinAll - pPad;
  const pMax = pMaxAll + pPad;
  const pRange = pMax - pMin || 1;
  const yTop = (v: number) =>
    PAD_T + (1 - (v - pMin) / pRange) * (H_TOP - PAD_T - PAD_B);

  const pricePath = ticks
    .map((t, i) => `${i === 0 ? "M" : "L"}${x(t.t).toFixed(1)},${yTop(t.price).toFixed(1)}`)
    .join(" ");

  const pxDigits = pMax > 100 ? 2 : pMax > 1 ? 4 : 6;
  const yTopTicks: number[] = [];
  for (let i = 0; i <= 3; i++) yTopTicks.push(pMin + (pRange * i) / 3);

  // ---- Bottom pane: balance + target + tolerance band ----
  const bals = ticks.map((t) => t.balance);
  const c = hedging.config;
  const upper = c.targetBalance + c.tolerance;
  const lower = Math.max(0, c.targetBalance - c.tolerance);
  const bMinAll = Math.min(...bals, lower);
  const bMaxAll = Math.max(...bals, upper);
  const bPad = (bMaxAll - bMinAll) * 0.15 || 1;
  const bMin = Math.max(0, bMinAll - bPad);
  const bMax = bMaxAll + bPad;
  const bRange = bMax - bMin || 1;
  const bY0 = H_TOP + GAP;
  const yBot = (v: number) =>
    bY0 + PAD_T + (1 - (v - bMin) / bRange) * (H_BOT - PAD_T - PAD_B);

  const balPath = ticks
    .map((t, i) => `${i === 0 ? "M" : "L"}${x(t.t).toFixed(1)},${yBot(t.balance).toFixed(1)}`)
    .join(" ");

  const bandTopY = yBot(upper);
  const bandBotY = yBot(lower);
  const bandX = PAD_L;
  const bandW = W - PAD_L - PAD_R;

  const balDigits = bMax > 100 ? 2 : bMax > 1 ? 3 : 4;
  const yBotTicks: number[] = [];
  for (let i = 0; i <= 3; i++) yBotTicks.push(bMin + (bRange * i) / 3);

  const lastPrice = ticks[ticks.length - 1].price;
  const lastBalance = ticks[ticks.length - 1].balance;

  return (
    <svg viewBox={`0 0 ${W + 90} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 420 }}>
      <rect x={0} y={0} width={W + 90} height={H} fill="transparent" />

      {/* ===== TOP PANE: price ===== */}
      {yTopTicks.map((v, i) => (
        <g key={`pt-${i}`}>
          <line x1={PAD_L} x2={W - PAD_R} y1={yTop(v)} y2={yTop(v)} stroke="#22222c" strokeWidth={1} />
          <text x={PAD_L - 8} y={yTop(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">
            {v.toFixed(pxDigits)}
          </text>
        </g>
      ))}
      <text x={PAD_L} y={PAD_T - 2} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">
        mid / hedge fills
      </text>

      <path d={pricePath} stroke="#ececf1" strokeWidth={1.6} fill="none" />

      {/* Hedge orders on price pane: circle open, disc filled, colored by side */}
      {hedging.orders.map((o: HedgeOrder) => {
        const color = o.side === "BID" ? "var(--pos)" : "var(--neg)";
        const fillCol = o.status === "filled" ? color : "var(--bg-card)";
        const cx = x(o.filledAt ?? o.t);
        const cy = yTop(o.filledPrice ?? o.price);
        return (
          <g key={`o-${o.seq}`}>
            {/* Vertical marker at time of placement */}
            <line
              x1={x(o.t)}
              x2={x(o.t)}
              y1={PAD_T}
              y2={H_TOP - PAD_B}
              stroke={color}
              strokeWidth={0.6}
              opacity={0.25}
              strokeDasharray="2,3"
            />
            <circle cx={cx} cy={cy} r={4.5} fill={fillCol} stroke={color} strokeWidth={1.6} />
            {o.status === "filled" && (
              <text x={cx + 6} y={cy - 6} fill={color} fontSize={9} fontFamily="ui-monospace, monospace">
                {o.side} {o.qty}
              </text>
            )}
          </g>
        );
      })}

      <circle cx={x(tMax)} cy={yTop(lastPrice)} r={3.5} fill="#ececf1" />
      <text x={W - PAD_R + 4} y={yTop(lastPrice) + 3} fill="#ececf1" fontSize={10} fontFamily="ui-monospace, monospace">
        {lastPrice.toFixed(pxDigits)}
      </text>

      {/* ===== BOTTOM PANE: balance + target band ===== */}
      {/* Tolerance band (green zone) */}
      <rect
        x={bandX}
        y={Math.min(bandTopY, bandBotY)}
        width={bandW}
        height={Math.abs(bandBotY - bandTopY)}
        fill="var(--pos)"
        opacity={0.08}
      />
      <line x1={bandX} x2={bandX + bandW} y1={bandTopY} y2={bandTopY} stroke="var(--pos)" strokeWidth={1} strokeDasharray="3,3" opacity={0.6} />
      <line x1={bandX} x2={bandX + bandW} y1={bandBotY} y2={bandBotY} stroke="var(--pos)" strokeWidth={1} strokeDasharray="3,3" opacity={0.6} />

      {/* Target line */}
      <line
        x1={bandX}
        x2={bandX + bandW}
        y1={yBot(c.targetBalance)}
        y2={yBot(c.targetBalance)}
        stroke="var(--accent)"
        strokeWidth={1.2}
        strokeDasharray="4,3"
      />

      {yBotTicks.map((v, i) => (
        <g key={`bt-${i}`}>
          <line x1={PAD_L} x2={W - PAD_R} y1={yBot(v)} y2={yBot(v)} stroke="#22222c" strokeWidth={1} opacity={0.35} />
          <text x={PAD_L - 8} y={yBot(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">
            {v.toFixed(balDigits)}
          </text>
        </g>
      ))}
      <text x={PAD_L} y={bY0 + PAD_T - 2} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">
        balance vs target
      </text>

      {/* Balance line */}
      <path d={balPath} stroke="var(--accent)" strokeWidth={1.6} fill="none" />

      {/* Hedge placement markers on balance pane (arrows) */}
      {hedging.orders.map((o: HedgeOrder) => {
        const color = o.side === "BID" ? "var(--pos)" : "var(--neg)";
        const cx = x(o.t);
        const cy = yBot(c.targetBalance);
        // Arrow: BID = up (buying pushes balance up), OFFER = down.
        const dy = o.side === "BID" ? -10 : 10;
        return (
          <g key={`m-${o.seq}`} opacity={o.status === "filled" ? 1 : 0.6}>
            <line x1={cx} x2={cx} y1={cy - dy} y2={cy + dy} stroke={color} strokeWidth={1.5} />
            <polygon
              points={
                o.side === "BID"
                  ? `${cx - 3},${cy - dy + 4} ${cx + 3},${cy - dy + 4} ${cx},${cy - dy - 2}`
                  : `${cx - 3},${cy + dy - 4} ${cx + 3},${cy + dy - 4} ${cx},${cy + dy + 2}`
              }
              fill={color}
            />
          </g>
        );
      })}

      <circle cx={x(tMax)} cy={yBot(lastBalance)} r={3.5} fill="var(--accent)" />
      <text x={W - PAD_R + 4} y={yBot(lastBalance) + 3} fill="var(--accent)" fontSize={10} fontFamily="ui-monospace, monospace">
        {lastBalance.toFixed(balDigits)}
      </text>
      <text x={W - PAD_R + 4} y={yBot(c.targetBalance) + 3} fill="var(--accent)" fontSize={10} opacity={0.7} fontFamily="ui-monospace, monospace">
        target
      </text>
    </svg>
  );
}
