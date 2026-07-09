"use client";

import type { Tick } from "@/lib/store";
import type { PairsTradingState, PairsOrder } from "@/lib/pairstrading-engine";

type Props = Readonly<{ ticks: readonly Tick[]; pairstrading: PairsTradingState | null }>;

const W = 720, PAD_L = 60, PAD_R = 60, PAD_T = 14, PAD_B = 26;
const H_TOP = 180;   // normalized prices
const H_BOT = 150;   // ratio + z-score bands
const GAP = 18;
const H_TOTAL = H_TOP + GAP + H_BOT;

export function PairsTradingChart({ ticks, pairstrading }: Props) {
  if (ticks.length === 0 || !pairstrading) {
    return (
      <div style={{ height: H_TOTAL, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>
        No data — start Pairs Trading (z-score) to see leg prices, ratio and z-bands.
      </div>
    );
  }

  const startA = pairstrading.config.startingPriceA;
  const startB = pairstrading.config.startingPriceB;
  const entryZ = pairstrading.config.entryZ;
  const mean = pairstrading.mean;
  const sigma = pairstrading.sigma;
  const haveBands = mean !== null && sigma !== null && sigma > 0;
  const upperBand = haveBands ? (mean as number) + entryZ * (sigma as number) : null;
  const lowerBand = haveBands ? (mean as number) - entryZ * (sigma as number) : null;

  // Normalize prices to 1.0 at start so both legs share a scale.
  const normA = ticks.map((t) => t.priceA / startA);
  const normB = ticks.map((t) => t.priceB / startB);
  const ratios = ticks.map((t) => t.ratio);

  // Top pane bounds (normalized prices)
  const topMinRaw = Math.min(...normA, ...normB);
  const topMaxRaw = Math.max(...normA, ...normB);
  const topPad = (topMaxRaw - topMinRaw) * 0.15 || 0.02;
  const topMin = topMinRaw - topPad;
  const topMax = topMaxRaw + topPad;
  const topRange = topMax - topMin || 1;

  // Bottom pane bounds (ratio) — include μ and z-bands when available
  const botCandidates = [...ratios];
  if (mean !== null) botCandidates.push(mean);
  if (upperBand !== null) botCandidates.push(upperBand);
  if (lowerBand !== null) botCandidates.push(lowerBand);
  const botMinRaw = Math.min(...botCandidates);
  const botMaxRaw = Math.max(...botCandidates);
  const botPad = (botMaxRaw - botMinRaw) * 0.15 || (mean ?? ratios[0]) * 0.02;
  const botMin = botMinRaw - botPad;
  const botMax = botMaxRaw + botPad;
  const botRange = botMax - botMin || 1;

  const tMin = ticks[0].t;
  const tMax = ticks[ticks.length - 1].t;
  const tRange = tMax - tMin || 1;
  const x = (t: number) => PAD_L + ((t - tMin) / tRange) * (W - PAD_L - PAD_R);
  const yTop = (v: number) => PAD_T + (1 - (v - topMin) / topRange) * (H_TOP - PAD_T - PAD_B);
  const yBot = (v: number) => H_TOP + GAP + PAD_T + (1 - (v - botMin) / botRange) * (H_BOT - PAD_T - PAD_B);

  const buildPath = (values: number[], yFn: (v: number) => number) =>
    ticks.map((t, i) => `${i === 0 ? "M" : "L"}${x(t.t).toFixed(1)},${yFn(values[i]).toFixed(1)}`).join(" ");

  const pathA = buildPath(normA, yTop);
  const pathB = buildPath(normB, yTop);
  const pathRatio = buildPath(ratios, yBot);

  // Grid ticks (5 per pane)
  const topTicks: number[] = [];
  for (let i = 0; i <= 4; i++) topTicks.push(topMin + (topRange * i) / 4);
  const botTicks: number[] = [];
  for (let i = 0; i <= 4; i++) botTicks.push(botMin + (botRange * i) / 4);

  const lastA = ticks[ticks.length - 1].priceA;
  const lastB = ticks[ticks.length - 1].priceB;
  const lastRatio = ticks[ticks.length - 1].ratio;

  return (
    <svg viewBox={`0 0 ${W + 30} ${H_TOTAL}`} style={{ width: "100%", height: "auto", maxHeight: 420 }}>
      {/* Top pane: normalized price A and B */}
      {topTicks.map((v, i) => (
        <g key={`gt${i}`}>
          <line x1={PAD_L} x2={W - PAD_R} y1={yTop(v)} y2={yTop(v)} stroke="#22222c" strokeWidth={1} />
          <text x={PAD_L - 8} y={yTop(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{v.toFixed(3)}×</text>
        </g>
      ))}
      <text x={PAD_L} y={PAD_T - 2} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">normalized price (starts at 1.00×)</text>
      <path d={pathA} stroke="#f9a03f" strokeWidth={1.6} fill="none" opacity={0.92} />
      <path d={pathB} stroke="#4dabf7" strokeWidth={1.6} fill="none" opacity={0.92} />

      {/* Signal markers on the top pane at leg-A price */}
      {pairstrading.orders.filter((o) => o.leg === "A").map((o: PairsOrder) => {
        const color = o.side === "BID" ? "var(--pos)" : "var(--neg)";
        const fillCol = o.status === "filled" ? color : "var(--bg-card)";
        return (
          <g key={`a-${o.seq}`}>
            <circle cx={x(o.t)} cy={yTop(o.price / startA)} r={4.5} fill={fillCol} stroke={color} strokeWidth={1.6} />
          </g>
        );
      })}
      {/* Leg B markers */}
      {pairstrading.orders.filter((o) => o.leg === "B").map((o: PairsOrder) => {
        const color = o.side === "BID" ? "var(--pos)" : "var(--neg)";
        const fillCol = o.status === "filled" ? color : "var(--bg-card)";
        return (
          <g key={`b-${o.seq}`}>
            <rect x={x(o.t) - 4} y={yTop(o.price / startB) - 4} width={8} height={8} fill={fillCol} stroke={color} strokeWidth={1.6} />
          </g>
        );
      })}

      <circle cx={x(tMax)} cy={yTop(lastA / startA)} r={3.5} fill="#f9a03f" />
      <circle cx={x(tMax)} cy={yTop(lastB / startB)} r={3.5} fill="#4dabf7" />
      <text x={W - PAD_R + 4} y={yTop(lastA / startA) + 3} fill="#f9a03f" fontSize={10} fontFamily="ui-monospace, monospace">A {lastA.toFixed(4)}</text>
      <text x={W - PAD_R + 4} y={yTop(lastB / startB) + 3} fill="#4dabf7" fontSize={10} fontFamily="ui-monospace, monospace">B {lastB.toFixed(4)}</text>

      {/* Bottom pane: ratio + rolling μ + z-bands */}
      {botTicks.map((v, i) => (
        <g key={`gb${i}`}>
          <line x1={PAD_L} x2={W - PAD_R} y1={yBot(v)} y2={yBot(v)} stroke="#22222c" strokeWidth={1} />
          <text x={PAD_L - 8} y={yBot(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{v.toFixed(4)}</text>
        </g>
      ))}
      <text x={PAD_L} y={H_TOP + GAP + PAD_T - 2} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">
        ratio = mid_A / mid_B {haveBands ? `(μ=${(mean as number).toFixed(4)}, ±${entryZ.toFixed(2)}σ = ±${((sigma as number) * entryZ).toFixed(4)})` : "(warming up)"}
      </text>

      {/* z-bands and mean */}
      {upperBand !== null && (
        <line x1={PAD_L} x2={W - PAD_R} y1={yBot(upperBand)} y2={yBot(upperBand)} stroke="#3f5b8a" strokeWidth={1} strokeDasharray="3,3" opacity={0.8} />
      )}
      {lowerBand !== null && (
        <line x1={PAD_L} x2={W - PAD_R} y1={yBot(lowerBand)} y2={yBot(lowerBand)} stroke="#3f5b8a" strokeWidth={1} strokeDasharray="3,3" opacity={0.8} />
      )}
      {mean !== null && (
        <line x1={PAD_L} x2={W - PAD_R} y1={yBot(mean)} y2={yBot(mean)} stroke="var(--accent)" strokeWidth={1.4} opacity={0.9} />
      )}

      {/* Ratio path */}
      <path d={pathRatio} stroke="#ececf1" strokeWidth={1.6} fill="none" />

      {/* Signal markers on ratio pane */}
      {pairstrading.orders.filter((o) => o.leg === "A").map((o: PairsOrder) => {
        const ratioAtT = ticks.find((t) => t.t === o.t)?.ratio;
        if (ratioAtT === undefined) return null;
        const color = o.side === "OFFER" ? "var(--neg)" : "var(--pos)";
        const fillCol = o.status === "filled" ? color : "var(--bg-card)";
        return (
          <g key={`r-${o.seq}`}>
            <circle cx={x(o.t)} cy={yBot(ratioAtT)} r={4} fill={fillCol} stroke={color} strokeWidth={1.6} />
          </g>
        );
      })}

      <circle cx={x(tMax)} cy={yBot(lastRatio)} r={3.5} fill="#ececf1" />
      <text x={W - PAD_R + 4} y={yBot(lastRatio) + 3} fill="#ececf1" fontSize={10} fontFamily="ui-monospace, monospace">{lastRatio.toFixed(4)}</text>
      {mean !== null && (
        <text x={W - PAD_R + 4} y={yBot(mean) + 3} fill="var(--accent)" fontSize={10} fontFamily="ui-monospace, monospace">μ</text>
      )}
    </svg>
  );
}
