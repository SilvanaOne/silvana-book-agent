"use client";

import type { Tick } from "@/lib/store";
import type { PositionState } from "@/lib/tpsl-engine";

type Props = Readonly<{
  ticks: readonly Tick[];
  position: PositionState | null;
}>;

const W = 720;
const H = 300;
const PAD_L = 60;
const PAD_R = 20;
const PAD_T = 14;
const PAD_B = 26;

export function PriceChart({ ticks, position }: Props) {
  if (ticks.length === 0 || !position) {
    return (
      <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "#6c7fa0" }}>
        No data yet — start a position to see live price.
      </div>
    );
  }
  const prices = ticks.map((t) => t.price);
  const lines: number[] = [];
  if (position.config.tp !== null) lines.push(position.config.tp);
  if (position.currentSl !== null) lines.push(position.currentSl);
  lines.push(position.config.entryPrice);

  const minAll = Math.min(...prices, ...lines);
  const maxAll = Math.max(...prices, ...lines);
  const pad = (maxAll - minAll) * 0.1 || minAll * 0.01;
  const yMin = minAll - pad;
  const yMax = maxAll + pad;
  const yRange = yMax - yMin || 1;

  const x = (i: number) => PAD_L + (i / Math.max(ticks.length - 1, 1)) * (W - PAD_L - PAD_R);
  const y = (v: number) => PAD_T + (1 - (v - yMin) / yRange) * (H - PAD_T - PAD_B);

  const pathD = ticks
    .map((t, i) => `${i === 0 ? "M" : "L"}${x(i).toFixed(1)},${y(t.price).toFixed(1)}`)
    .join(" ");

  const digits = yMax > 100 ? 2 : yMax > 1 ? 4 : 6;
  const yTicks: number[] = [];
  for (let i = 0; i <= 4; i++) yTicks.push(yMin + (yRange * i) / 4);

  const lastPrice = ticks[ticks.length - 1].price;

  const HLine = ({ v, color, label, dashed }: { v: number; color: string; label: string; dashed?: boolean }) => (
    <>
      <line
        x1={PAD_L}
        x2={W - PAD_R}
        y1={y(v)}
        y2={y(v)}
        stroke={color}
        strokeWidth={1}
        strokeDasharray={dashed ? "4,4" : undefined}
        opacity={0.7}
      />
      <text x={W - PAD_R + 2} y={y(v) + 3} fill={color} fontSize={10} fontFamily="ui-monospace, monospace">
        {label}
      </text>
    </>
  );

  const showTp = position.config.tp !== null;
  const showSl = position.currentSl !== null;

  return (
    <svg viewBox={`0 0 ${W + 80} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 340 }}>
      <rect x={0} y={0} width={W + 80} height={H} fill="#0b1020" />
      {/* Y axis grid + labels */}
      {yTicks.map((v, i) => (
        <g key={i}>
          <line x1={PAD_L} x2={W - PAD_R} y1={y(v)} y2={y(v)} stroke="#1c2647" strokeWidth={1} />
          <text x={PAD_L - 8} y={y(v) + 3} fill="#6c7fa0" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">
            {v.toFixed(digits)}
          </text>
        </g>
      ))}

      {/* Entry line */}
      <HLine v={position.config.entryPrice} color="#6c7fa0" label={`entry ${position.config.entryPrice.toFixed(digits)}`} dashed />
      {/* TP line */}
      {showTp && <HLine v={position.config.tp!} color="#6de6a3" label={`TP ${position.config.tp!.toFixed(digits)}`} />}
      {/* SL line */}
      {showSl && <HLine v={position.currentSl!} color="#ff8dab" label={`SL ${position.currentSl!.toFixed(digits)}`} />}

      {/* Price line */}
      <path d={pathD} stroke="#3b7dfb" strokeWidth={1.8} fill="none" />
      {/* Last-price dot */}
      <circle cx={x(ticks.length - 1)} cy={y(lastPrice)} r={3.5} fill="#3b7dfb" />

      {/* Trigger marker */}
      {position.status === "triggered" && position.triggeredAt !== null && (() => {
        const trig = ticks.find((t) => t.t >= (position.triggeredAt as number));
        if (!trig) return null;
        const i = ticks.indexOf(trig);
        return (
          <g>
            <circle cx={x(i)} cy={y(trig.price)} r={7} fill={position.triggeredReason === "TAKE PROFIT" ? "#6de6a3" : "#ff8dab"} opacity={0.35} />
            <circle cx={x(i)} cy={y(trig.price)} r={4} fill={position.triggeredReason === "TAKE PROFIT" ? "#6de6a3" : "#ff8dab"} />
          </g>
        );
      })()}
    </svg>
  );
}
