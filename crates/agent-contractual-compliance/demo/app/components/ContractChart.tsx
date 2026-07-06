"use client";

import type { ContractSnapshot, ContractState } from "@/lib/contract-engine";

type Props = Readonly<{ agent: ContractState | null }>;

const W = 720, H = 380;

export function ContractChart({ agent }: Props) {
  if (!agent) return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start tracking.</div>;
  const snaps = agent.snapshots;
  if (snaps.length === 0) return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>Waiting for first status cycle…</div>;

  const rowH = Math.min(70, (H - 40) / Math.max(snaps.length, 1));
  const barX = 210;
  const barMax = W - barX - 80;

  return (
    <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 400 }}>
      <rect x={0} y={0} width={W} height={H} fill="transparent" />
      <text x={10} y={20} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">
        contract windows · cycle #{agent.cycle}
      </text>

      {snaps.map((s: ContractSnapshot, i: number) => {
        const y = 40 + i * rowH;
        const c = s.contract;
        const min = c.minNotional ?? 0;
        const max = c.maxNotional ?? Math.max(s.totalInWindow * 1.2, 1);
        const range = Math.max(max * 1.2, 1);
        const minX = barX + (min / range) * barMax;
        const maxX = barX + Math.min(1, (c.maxNotional ?? range) / range) * barMax;
        const curX = barX + Math.min(1, s.totalInWindow / range) * barMax;
        const stateColor = s.overCeiling || s.underFloor ? "var(--neg)" : s.expired ? "var(--text-faint)" : "var(--pos)";

        return (
          <g key={c.id}>
            {/* contract id */}
            <text x={10} y={y + 14} fill="var(--text)" fontSize={12} fontFamily="ui-monospace, monospace">{truncate(c.id, 22)}</text>
            <text x={10} y={y + 28} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">
              {truncate(c.counterparty, 22)}
            </text>
            <text x={10} y={y + 42} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">
              {c.market} · {c.windowHours}h · min {c.minNotional ?? "-"} max {c.maxNotional ?? "-"}
            </text>

            {/* track */}
            <rect x={barX} y={y + 20} width={barMax} height={16} rx={3} fill="var(--bg-card)" stroke="var(--border)" strokeWidth={0.6} />
            {/* min-max band (if both set) */}
            {c.minNotional !== undefined && c.maxNotional !== undefined && (
              <rect x={minX} y={y + 20} width={maxX - minX} height={16} fill="rgba(34,197,94,0.15)" />
            )}
            {/* threshold lines */}
            {c.minNotional !== undefined && (
              <line x1={minX} x2={minX} y1={y + 16} y2={y + 40} stroke="var(--pos)" strokeWidth={1} strokeDasharray="3 3" />
            )}
            {c.maxNotional !== undefined && (
              <line x1={maxX} x2={maxX} y1={y + 16} y2={y + 40} stroke="var(--neg)" strokeWidth={1} strokeDasharray="3 3" />
            )}
            {/* current marker */}
            <line x1={curX} x2={curX} y1={y + 18} y2={y + 40} stroke={stateColor} strokeWidth={2} />
            <circle cx={curX} cy={y + 28} r={4} fill={stateColor} />

            {/* status label */}
            <text x={W - 10} y={y + 32} fill={stateColor} fontSize={11} fontFamily="ui-monospace, monospace" textAnchor="end">
              {s.expired ? "EXPIRED" : s.underFloor ? "UNDER" : s.overCeiling ? "OVER" : "OK"} · {s.totalInWindow.toFixed(0)}
            </text>
            <text x={W - 10} y={y + 45} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="end">
              {s.entries.length} settles in window
            </text>
          </g>
        );
      })}
    </svg>
  );
}

function truncate(s: string, n: number): string { return s.length <= n ? s : s.slice(0, n - 1) + "…"; }
