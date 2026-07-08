"use client";

import type { DiscloseState } from "@/lib/disclose-engine";

type Props = Readonly<{ agent: DiscloseState | null }>;

const W = 720, H = 400;

export function DiscloseChart({ agent }: Props) {
  if (!agent) return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — load a history to filter.</div>;
  const disc = agent.disclosure.slice(-10);
  if (disc.length === 0) return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>Filter matched zero records. Loosen filters.</div>;

  return (
    <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 420 }}>
      <rect x={0} y={0} width={W} height={H} fill="transparent" />
      <text x={10} y={20} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">
        disclosure log · last {disc.length} of {agent.disclosure.length} records
      </text>

      {disc.map((rec, i) => {
        const y = 32 + i * 36;
        const isSettle = rec.kind.startsWith("settlement.");
        const color = isSettle ? "var(--pos)" : "var(--accent)";
        return (
          <g key={rec.seq}>
            <rect x={10} y={y} width={W - 20} height={32} rx={4} fill="var(--bg-card)" stroke="var(--border)" strokeWidth={0.6} />
            <rect x={10} y={y} width={4} height={32} fill={color} />
            <text x={20} y={y + 12} fill="var(--text)" fontSize={11} fontFamily="ui-monospace, monospace">seq={rec.seq} · {rec.kind}</text>
            <text x={20} y={y + 26} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">
              payload={truncate(JSON.stringify(rec.payload), 68)}
            </text>
            <text x={W - 14} y={y + 12} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="end">
              prev_hash {rec.prev_hash.slice(0, 8)}…{rec.prev_hash.slice(-4)}
            </text>
            <text x={W - 14} y={y + 26} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="end">
              sig {rec.signature.slice(0, 10)}…
            </text>
          </g>
        );
      })}
    </svg>
  );
}

function truncate(s: string, n: number): string { return s.length <= n ? s : s.slice(0, n - 1) + "…"; }
