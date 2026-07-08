"use client";

import type { Tick } from "@/lib/store";
import type { SignalBotState, SignalOrder } from "@/lib/signalbot-engine";

type Props = Readonly<{ ticks: readonly Tick[]; signalbot: SignalBotState | null }>;

const W = 720, H = 300, PAD_L = 60, PAD_R = 20, PAD_T = 14, PAD_B = 26;
const BAR_H = 92, BAR_PAD_T = 14;

export function SignalBotChart({ ticks, signalbot }: Props) {
  if (ticks.length === 0 || !signalbot) {
    return (
      <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>
        No data — start Signal Bot to see mid, ingested signals, and cursor progress.
      </div>
    );
  }

  const prices = ticks.map((t) => t.price);
  const signalPrices = signalbot.signalOrders.map((o) => o.signal.price);
  const minAll = Math.min(...prices, ...signalPrices);
  const maxAll = Math.max(...prices, ...signalPrices);
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

  // Bottom row: cursor progress + stats bars
  const stats = signalbot.stats;
  const totalOrders = Math.max(1, stats.submitted + stats.rejected);
  const filledPct = stats.filled / totalOrders;
  const rejectedPct = stats.rejected / totalOrders;
  const cursorBarMax = 200_000;
  const cursorPct = Math.min(1, signalbot.cursorBytes / cursorBarMax);

  return (
    <svg viewBox={`0 0 ${W + 90} ${H + BAR_H}`} style={{ width: "100%", height: "auto", maxHeight: 460 }}>
      <rect x={0} y={0} width={W + 90} height={H + BAR_H} fill="transparent" />

      {/* Top: price chart */}
      {yTicks.map((v, i) => (
        <g key={i}>
          <line x1={PAD_L} x2={W - PAD_R} y1={y(v)} y2={y(v)} stroke="#22222c" strokeWidth={1} />
          <text x={PAD_L - 8} y={y(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{v.toFixed(digits)}</text>
        </g>
      ))}

      {/* Mid price */}
      <path d={pricePath} stroke="#ececf1" strokeWidth={1.6} fill="none" />

      {/* Signal markers: small orange dots at (receivedAt, signal.price). */}
      {signalbot.signalOrders.map((o: SignalOrder) => (
        <circle key={`sig-${o.seq}`} cx={x(o.signal.receivedAt)} cy={y(o.signal.price)} r={3.2} fill="#ff9a2e" opacity={0.85} />
      ))}

      {/* Order dots + arrows from signal → order execution */}
      {signalbot.signalOrders.map((o: SignalOrder) => {
        const isBid = o.signal.side === "buy";
        const color = o.status === "rejected" ? "#c04b4b" : isBid ? "var(--pos)" : "var(--neg)";
        const fillCol = o.status === "filled" ? color : o.status === "rejected" ? color : "var(--bg-card)";
        const oxT = o.filledAt ?? o.t;
        const oxP = o.filledPrice ?? o.signal.price;
        const x1 = x(o.signal.receivedAt), y1 = y(o.signal.price);
        const x2 = x(oxT), y2 = y(oxP);
        return (
          <g key={`ord-${o.seq}`}>
            <line x1={x1} y1={y1} x2={x2} y2={y2} stroke={color} strokeWidth={0.9} strokeDasharray="2,2" opacity={0.55} />
            <circle cx={x2} cy={y2} r={4.5} fill={fillCol} stroke={color} strokeWidth={1.6} />
          </g>
        );
      })}

      <circle cx={x(tMax)} cy={y(lastPrice)} r={3.5} fill="#ececf1" />

      {/* Bottom: cursor progress + stats bars */}
      <g transform={`translate(0, ${H + BAR_PAD_T})`}>
        <text x={PAD_L} y={0} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">CURSOR {signalbot.cursorBytes.toLocaleString()} bytes</text>
        <rect x={PAD_L} y={6} width={W - PAD_L - PAD_R} height={10} fill="#1a1a22" stroke="#22222c" strokeWidth={1} />
        <rect x={PAD_L} y={6} width={(W - PAD_L - PAD_R) * cursorPct} height={10} fill="var(--accent)" opacity={0.8} />

        <text x={PAD_L} y={36} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">FILLED / SUBMITTED / REJECTED</text>
        <g transform={`translate(${PAD_L}, 42)`}>
          <rect x={0} y={0} width={(W - PAD_L - PAD_R) * filledPct} height={10} fill="var(--pos)" opacity={0.85} />
          <rect x={(W - PAD_L - PAD_R) * filledPct} y={0} width={(W - PAD_L - PAD_R) * Math.max(0, 1 - filledPct - rejectedPct)} height={10} fill="#5a5a68" opacity={0.6} />
          <rect x={(W - PAD_L - PAD_R) * (1 - rejectedPct)} y={0} width={(W - PAD_L - PAD_R) * rejectedPct} height={10} fill="var(--neg)" opacity={0.85} />
        </g>
        <text x={W - PAD_R} y={64} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="end">
          submitted={stats.submitted} filled={stats.filled} rejected={stats.rejected}
        </text>
      </g>
    </svg>
  );
}
