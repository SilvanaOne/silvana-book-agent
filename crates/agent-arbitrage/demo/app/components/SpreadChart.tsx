"use client";

import type { ArbitrageState, SpreadPoint } from "@/lib/arbitrage-engine";

type Props = Readonly<{ arbitrage: ArbitrageState | null }>;

const W = 720, H = 300, PAD_L = 52, PAD_R = 20, PAD_T = 14, PAD_B = 26;

export function SpreadChart({ arbitrage }: Props) {
  const series: readonly SpreadPoint[] = arbitrage?.series ?? [];
  if (!arbitrage || series.length === 0) {
    return (
      <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>
        No data — start the scanner to plot the {arbitrage?.config.focusPair ?? "focus pair"} spread.
      </div>
    );
  }

  const threshold = arbitrage.config.minSpreadBps;
  const values = series.map((p) => p.bps);
  const minAll = Math.min(...values, threshold);
  const maxAll = Math.max(...values, threshold);
  const pad = (maxAll - minAll) * 0.12 || 10;
  const yMin = Math.max(0, minAll - pad), yMax = maxAll + pad, yRange = yMax - yMin || 1;
  const tMin = series[0]!.t, tMax = series[series.length - 1]!.t;
  const tRange = tMax - tMin || 1;
  const x = (t: number) => PAD_L + ((t - tMin) / tRange) * (W - PAD_L - PAD_R);
  const y = (v: number) => PAD_T + (1 - (v - yMin) / yRange) * (H - PAD_T - PAD_B);

  const line = series.map((p, i) => `${i === 0 ? "M" : "L"}${x(p.t).toFixed(1)},${y(p.bps).toFixed(1)}`).join(" ");
  const area = `${line} L${x(tMax).toFixed(1)},${y(yMin).toFixed(1)} L${x(tMin).toFixed(1)},${y(yMin).toFixed(1)} Z`;

  const yTicks: number[] = [];
  for (let i = 0; i <= 4; i++) yTicks.push(yMin + (yRange * i) / 4);

  const last = series[series.length - 1]!;

  return (
    <svg viewBox={`0 0 ${W + 70} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 340 }}>
      <defs>
        <linearGradient id="spreadFill" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor="var(--accent)" stopOpacity={0.22} />
          <stop offset="100%" stopColor="var(--accent)" stopOpacity={0} />
        </linearGradient>
      </defs>

      {yTicks.map((v, i) => (
        <g key={i}>
          <line x1={PAD_L} x2={W - PAD_R} y1={y(v)} y2={y(v)} stroke="#22222c" strokeWidth={1} />
          <text x={PAD_L - 8} y={y(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{v.toFixed(0)}</text>
        </g>
      ))}

      {/* Act threshold. */}
      <line x1={PAD_L} x2={W - PAD_R} y1={y(threshold)} y2={y(threshold)} stroke="var(--pos)" strokeWidth={1.2} strokeDasharray="5 4" opacity={0.8} />
      <text x={W - PAD_R + 4} y={y(threshold) + 3} fill="var(--pos)" fontSize={10} fontFamily="ui-monospace, monospace">act {threshold}</text>

      {/* Spread area + line. */}
      <path d={area} fill="url(#spreadFill)" stroke="none" />
      <path d={line} stroke="var(--accent)" strokeWidth={1.7} fill="none" />

      {/* Acted markers. */}
      {series
        .filter((p) => p.acted)
        .map((p, i) => (
          <circle key={`a-${i}`} cx={x(p.t)} cy={y(p.bps)} r={4} fill="var(--pos)" stroke="var(--pos)" strokeWidth={1.2} />
        ))}

      {/* Current value. */}
      <circle cx={x(tMax)} cy={y(last.bps)} r={3.5} fill="var(--accent)" />
      <text x={W - PAD_R + 4} y={y(last.bps) + 3} fill="var(--accent)" fontSize={10} fontFamily="ui-monospace, monospace">{Math.round(last.bps)} bps</text>

      <text x={PAD_L} y={H - 6} fill="var(--text-faint)" fontSize={9} fontFamily="ui-monospace, monospace">{arbitrage.config.focusPair} cross-venue spread (bps)</text>
    </svg>
  );
}
