"use client";

import type { AlgoState, StepProgress } from "@/lib/algo-engine";

type Props = Readonly<{ agent: AlgoState | null }>;

const W = 720, H = 340;

export function AlgoChart({ agent }: Props) {
  if (!agent) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start dispatcher to see plan progress.</div>;
  }
  const steps = agent.steps;
  const rowH = Math.min(56, (H - 40) / Math.max(steps.length, 1));

  return (
    <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 360 }}>
      <rect x={0} y={0} width={W} height={H} fill="transparent" />
      <text x={10} y={20} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">
        plan progress · mid {agent.currentPrice.toFixed(6)}
      </text>

      {steps.map((sp: StepProgress, i: number) => {
        const y = 40 + i * rowH;
        const barX = 160;
        const barW = W - barX - 80;
        const pct = Math.min(1, sp.total > 0 ? sp.filled / sp.total : 0);
        const color = sp.status === "done" ? "var(--pos)" : sp.status === "running" ? "var(--accent)" : "var(--text-faint)";
        const bgOpacity = sp.status === "running" ? 0.25 : 0.12;

        return (
          <g key={i}>
            {/* Step tag */}
            <text x={10} y={y + rowH / 2 - 4} fill="var(--text)" fontSize={12} fontFamily="ui-monospace, monospace">
              #{i + 1} {sp.algo}
            </text>
            <text x={10} y={y + rowH / 2 + 10} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">
              {sp.market} · {sp.side.toUpperCase()}
            </text>

            {/* Progress track */}
            <rect x={barX} y={y + 8} width={barW} height={rowH - 24} rx={4} fill={color} opacity={bgOpacity} />
            <rect x={barX} y={y + 8} width={barW * pct} height={rowH - 24} rx={4} fill={color} />

            {/* Labels */}
            <text x={barX + 6} y={y + rowH / 2 + 4} fill="var(--bg)" fontSize={11} fontFamily="ui-monospace, monospace">
              {sp.filled.toFixed(3)} / {sp.total.toFixed(3)}
            </text>
            <text x={W - 10} y={y + rowH / 2 + 4} fill={color} fontSize={11} fontFamily="ui-monospace, monospace" textAnchor="end">
              {(pct * 100).toFixed(1)}%
            </text>
            <text x={W - 10} y={y + rowH / 2 + 18} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="end">
              {sp.status} · {sp.childrenPlaced} placed
            </text>
          </g>
        );
      })}
    </svg>
  );
}
