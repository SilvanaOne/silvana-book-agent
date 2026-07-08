"use client";

import type { RetentionState } from "@/lib/retention-engine";

type Props = Readonly<{ agent: RetentionState | null }>;

const W = 720, H = 380;

export function RetentionChart({ agent }: Props) {
  if (!agent) return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — load a history to rotate.</div>;
  const slices = agent.slices;
  if (slices.length === 0) return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>All records were dropped by retention — increase the window.</div>;

  const brokenAt = agent.lastVerify && !agent.lastVerify.ok ? agent.lastVerify.brokenAt : undefined;
  const maxCount = Math.max(1, ...slices.map((s) => s.recordCount));

  return (
    <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 400 }}>
      <rect x={0} y={0} width={W} height={H} fill="transparent" />
      <text x={10} y={20} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">
        slice chain · {slices.length} slice(s) · {agent.totalRecords} records
      </text>
      {slices.slice(-10).map((s, i, arr) => {
        const y = 32 + i * 34;
        const w = (s.recordCount / maxCount) * (W - 320);
        const broken = brokenAt !== undefined && s.num >= brokenAt;
        const color = broken ? "var(--neg)" : "var(--pos)";
        return (
          <g key={s.num}>
            {/* prev arrow */}
            {i > 0 && <line x1={30} x2={30} y1={y - 12} y2={y - 4} stroke={brokenAt !== undefined && arr[i-1].num >= brokenAt ? "var(--neg)" : "var(--text-faint)"} strokeWidth={1} />}
            {/* slice card */}
            <rect x={10} y={y} width={W - 20} height={28} rx={4} fill="var(--bg-card)" stroke={color} strokeWidth={1} />
            <text x={20} y={y + 13} fill="var(--text)" fontSize={11} fontFamily="ui-monospace, monospace">slice #{s.num} · {s.bucket}</text>
            <text x={20} y={y + 24} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">
              prev {s.prevSliceHash ? s.prevSliceHash.slice(0, 12) + "…" : "(root)"} → hash {s.sliceHash.slice(0, 12)}…
            </text>
            {/* record-count bar */}
            <rect x={W - 230} y={y + 8} width={w} height={12} fill={color} opacity={0.6} />
            <text x={W - 12} y={y + 13} fill={color} fontSize={11} fontFamily="ui-monospace, monospace" textAnchor="end">{s.recordCount} recs</text>
            <text x={W - 12} y={y + 24} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="end">sig {s.signature.slice(0, 10)}…</text>
          </g>
        );
      })}
    </svg>
  );
}
