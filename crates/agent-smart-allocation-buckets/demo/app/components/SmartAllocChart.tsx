"use client";

import type { SmartAllocState } from "@/lib/smartalloc-engine";

type Props = Readonly<{ agent: SmartAllocState | null }>;

const W = 720, H = 380;

export function SmartAllocChart({ agent }: Props) {
  if (!agent || !agent.lastSnapshot) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start allocation to see bucket + instrument weights.</div>;
  }
  const snap = agent.lastSnapshot;
  const threshold = agent.config.bucketThresholdPct;

  const rows = snap.bucketRows;
  const barTrack = 460;              // width available for a weight bar
  const rowH = Math.min(80, (H - 40) / Math.max(rows.length, 1));

  return (
    <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 400 }}>
      <rect x={0} y={0} width={W} height={H} fill="transparent" />

      <text x={10} y={20} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">
        buckets — current vs target (±{threshold}% band) · portfolio {snap.portfolioValue.toFixed(2)}
      </text>

      {rows.map((r, i) => {
        const y = 40 + i * rowH;
        const barX = 130;
        const targetX = barX + r.targetWeight * barTrack;
        const currentX = barX + r.currentWeight * barTrack;
        const bandLeft = barX + Math.max(0, r.targetWeight - threshold / 100) * barTrack;
        const bandRight = barX + Math.min(1, r.targetWeight + threshold / 100) * barTrack;
        const color = r.breached ? "var(--neg)" : "var(--pos)";

        return (
          <g key={r.name}>
            {/* bucket name */}
            <text x={10} y={y + 14} fill="var(--text)" fontSize={12} fontFamily="ui-monospace, monospace">{r.name}</text>
            <text x={10} y={y + 30} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">tgt {(r.targetWeight * 100).toFixed(1)}%</text>

            {/* track */}
            <rect x={barX} y={y + 6} width={barTrack} height={14} fill="var(--bg-card)" stroke="var(--border)" strokeWidth={0.8} rx={2} />
            {/* threshold band */}
            <rect x={bandLeft} y={y + 6} width={Math.max(0, bandRight - bandLeft)} height={14} fill="rgba(255,138,28,0.15)" />
            {/* target marker */}
            <line x1={targetX} x2={targetX} y1={y + 4} y2={y + 22} stroke="var(--pos)" strokeWidth={1.2} />
            {/* current marker */}
            <line x1={currentX} x2={currentX} y1={y + 2} y2={y + 24} stroke={color} strokeWidth={2} />
            <circle cx={currentX} cy={y + 13} r={4} fill={color} />

            {/* right-side numeric */}
            <text x={barX + barTrack + 12} y={y + 14} fill={color} fontSize={11} fontFamily="ui-monospace, monospace">
              {(r.currentWeight * 100).toFixed(2)}%
            </text>
            <text x={barX + barTrack + 12} y={y + 30} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">
              Δ {r.deviationPct >= 0 ? "+" : ""}{r.deviationPct.toFixed(2)}%{r.breached ? "  BREACH" : ""}
            </text>

            {/* within-bucket instrument row */}
            {r.instruments.map((inst, j) => {
              const x = barX + j * 90;
              const yy = y + rowH - 26;
              return (
                <g key={inst.instrument}>
                  <text x={x} y={yy} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">
                    {truncate(`${inst.instrument}@${inst.market}`, 22)}
                  </text>
                  <text x={x} y={yy + 12} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">
                    bal {inst.balance.toFixed(3)} · mid {shortNum(inst.price)}
                  </text>
                </g>
              );
            })}
          </g>
        );
      })}

      {/* Legend */}
      <g transform={`translate(10, ${H - 6})`}>
        <line x1={0} x2={12} y1={-4} y2={-4} stroke="var(--pos)" strokeWidth={1.2} />
        <text x={16} y={-1} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">target</text>
        <circle cx={70} cy={-4} r={3.5} fill="var(--pos)" />
        <text x={78} y={-1} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">current (in band)</text>
        <circle cx={200} cy={-4} r={3.5} fill="var(--neg)" />
        <text x={208} y={-1} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">current (breach)</text>
        <rect x={330} y={-8} width={12} height={5} fill="rgba(255,138,28,0.4)" />
        <text x={346} y={-1} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">±threshold band</text>
      </g>
    </svg>
  );
}

function truncate(s: string, n: number): string {
  return s.length <= n ? s : s.slice(0, n - 1) + "…";
}

function shortNum(n: number): string {
  if (n >= 1000) return n.toFixed(0);
  if (n >= 1) return n.toFixed(2);
  return n.toFixed(6);
}
