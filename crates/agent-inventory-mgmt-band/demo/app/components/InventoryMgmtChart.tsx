"use client";

import type { Tick } from "@/lib/store";
import type { InventoryMgmtState, InventoryOrder } from "@/lib/inventorymgmt-engine";

type Props = Readonly<{ ticks: readonly Tick[]; inventorymgmt: InventoryMgmtState | null }>;

const W = 720, H = 320, PAD_L = 60, PAD_R = 60, PAD_T = 14, PAD_B = 26;
const PRICE_H = 140;   // top pane height
const GAP = 20;        // gap between panes
const BAL_TOP = PAD_T + PRICE_H + GAP;
const BAL_H = H - BAL_TOP - PAD_B;

export function InventoryMgmtChart({ ticks, inventorymgmt }: Props) {
  if (ticks.length === 0 || !inventorymgmt) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start Inventory Mgmt to see mid + balance vs target band.</div>;
  }
  const c = inventorymgmt.config;
  const prices = ticks.map((t) => t.price);
  const balances = ticks.map((t) => t.balance);
  const orderPrices = inventorymgmt.orders.map((o) => o.price);

  // Price pane range
  const pMin = Math.min(...prices, ...orderPrices);
  const pMax = Math.max(...prices, ...orderPrices);
  const pPad = (pMax - pMin) * 0.1 || pMin * 0.02 || 0.001;
  const priceMin = pMin - pPad, priceMax = pMax + pPad, priceRange = priceMax - priceMin || 1;

  // Balance pane range: include band
  const upper = c.target + c.tolerance;
  const lower = Math.max(0, c.target - c.tolerance);
  const bMin = Math.min(...balances, lower);
  const bMax = Math.max(...balances, upper);
  const bPad = (bMax - bMin) * 0.15 || Math.max(1, c.target * 0.05);
  const balMin = Math.max(0, bMin - bPad), balMax = bMax + bPad, balRange = balMax - balMin || 1;

  const tMin = ticks[0].t, tMax = ticks[ticks.length - 1].t;
  const tRange = tMax - tMin || 1;
  const x = (t: number) => PAD_L + ((t - tMin) / tRange) * (W - PAD_L - PAD_R);
  const yPrice = (v: number) => PAD_T + (1 - (v - priceMin) / priceRange) * PRICE_H;
  const yBal = (v: number) => BAL_TOP + (1 - (v - balMin) / balRange) * BAL_H;

  const pricePath = ticks.map((t, i) => `${i === 0 ? "M" : "L"}${x(t.t).toFixed(1)},${yPrice(t.price).toFixed(1)}`).join(" ");
  const balPath = ticks.map((t, i) => `${i === 0 ? "M" : "L"}${x(t.t).toFixed(1)},${yBal(t.balance).toFixed(1)}`).join(" ");

  const priceDigits = priceMax > 100 ? 2 : priceMax > 1 ? 4 : 6;
  const balDigits = balMax > 100 ? 1 : 3;
  const priceTicks: number[] = [];
  for (let i = 0; i <= 3; i++) priceTicks.push(priceMin + (priceRange * i) / 3);
  const balTicks: number[] = [];
  for (let i = 0; i <= 3; i++) balTicks.push(balMin + (balRange * i) / 3);

  const lastPrice = ticks[ticks.length - 1].price;
  const lastBal = ticks[ticks.length - 1].balance;

  // Band-exit markers: transitions in-band → out-of-band on balance.
  const exitMarkers: Array<{ t: number; balance: number }> = [];
  let prevInBand: boolean | null = null;
  for (const tk of ticks) {
    const inBand = tk.balance >= lower && tk.balance <= upper;
    if (prevInBand === true && !inBand) exitMarkers.push({ t: tk.t, balance: tk.balance });
    prevInBand = inBand;
  }

  return (
    <svg viewBox={`0 0 ${W + 90} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 360 }}>
      <rect x={0} y={0} width={W + 90} height={H} fill="transparent" />

      {/* Price pane title */}
      <text x={PAD_L} y={PAD_T - 2} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">MID</text>

      {/* Price gridlines */}
      {priceTicks.map((v, i) => (
        <g key={`p-${i}`}>
          <line x1={PAD_L} x2={W - PAD_R} y1={yPrice(v)} y2={yPrice(v)} stroke="#22222c" strokeWidth={1} />
          <text x={PAD_L - 8} y={yPrice(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{v.toFixed(priceDigits)}</text>
        </g>
      ))}

      {/* Mid price */}
      <path d={pricePath} stroke="#ececf1" strokeWidth={1.6} fill="none" />
      <circle cx={x(tMax)} cy={yPrice(lastPrice)} r={3.5} fill="#ececf1" />

      {/* Order price markers on price pane */}
      {inventorymgmt.orders.map((o: InventoryOrder) => {
        const color = o.side === "BID" ? "var(--pos)" : "var(--neg)";
        const fillCol = o.status === "filled" ? color : "var(--bg-card)";
        // Skip if order price is outside price pane
        if (o.price < priceMin || o.price > priceMax) return null;
        return (
          <g key={`op-${o.seq}`}>
            <circle cx={x(o.t)} cy={yPrice(o.price)} r={4.5} fill={fillCol} stroke={color} strokeWidth={1.6} />
          </g>
        );
      })}

      {/* Balance pane title */}
      <text x={PAD_L} y={BAL_TOP - 2} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">BALANCE ({c.instrument})</text>

      {/* Balance pane target band (green zone) */}
      <rect
        x={PAD_L}
        y={yBal(upper)}
        width={W - PAD_L - PAD_R}
        height={Math.max(0, yBal(lower) - yBal(upper))}
        fill="#1a3d2a"
        opacity={0.35}
      />
      {/* Target line */}
      <line x1={PAD_L} x2={W - PAD_R} y1={yBal(c.target)} y2={yBal(c.target)} stroke="var(--accent)" strokeWidth={1.2} strokeDasharray="4,3" />
      {/* Upper/lower bounds */}
      <line x1={PAD_L} x2={W - PAD_R} y1={yBal(upper)} y2={yBal(upper)} stroke="#6de6a3" strokeWidth={1} strokeDasharray="3,3" opacity={0.7} />
      <line x1={PAD_L} x2={W - PAD_R} y1={yBal(lower)} y2={yBal(lower)} stroke="#6de6a3" strokeWidth={1} strokeDasharray="3,3" opacity={0.7} />

      {/* Balance gridlines (right axis) */}
      {balTicks.map((v, i) => (
        <g key={`b-${i}`}>
          <line x1={PAD_L} x2={W - PAD_R} y1={yBal(v)} y2={yBal(v)} stroke="#22222c" strokeWidth={0.5} opacity={0.5} />
          <text x={PAD_L - 8} y={yBal(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{v.toFixed(balDigits)}</text>
        </g>
      ))}

      {/* Balance path */}
      <path d={balPath} stroke="#f0c674" strokeWidth={1.6} fill="none" />
      <circle cx={x(tMax)} cy={yBal(lastBal)} r={3.5} fill="#f0c674" />

      {/* Band-exit yellow markers on balance path */}
      {exitMarkers.map((m, i) => (
        <circle key={`ex-${i}`} cx={x(m.t)} cy={yBal(m.balance)} r={3.5} fill="#ffd166" stroke="#f0c674" strokeWidth={1} opacity={0.9} />
      ))}

      {/* Fill markers on balance pane (balanceAfter) */}
      {inventorymgmt.orders
        .filter((o) => o.status === "filled" && o.filledAt !== undefined)
        .map((o) => {
          const color = o.side === "BID" ? "var(--pos)" : "var(--neg)";
          return (
            <circle key={`bf-${o.seq}`} cx={x(o.filledAt!)} cy={yBal(o.balanceAfter)} r={3.5} fill={color} stroke="#0b0f19" strokeWidth={0.8} />
          );
        })}

      {/* Right-margin labels */}
      <text x={W - PAD_R + 4} y={yBal(c.target) + 3} fill="var(--accent)" fontSize={10} fontFamily="ui-monospace, monospace">TARGET {c.target}</text>
      <text x={W - PAD_R + 4} y={yBal(upper) + 3} fill="#6de6a3" fontSize={10} fontFamily="ui-monospace, monospace">+tol</text>
      <text x={W - PAD_R + 4} y={yBal(lower) + 3} fill="#6de6a3" fontSize={10} fontFamily="ui-monospace, monospace">−tol</text>
    </svg>
  );
}
