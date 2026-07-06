"use client";

import type { AttestState } from "@/lib/attest-engine";

type Props = Readonly<{ agent: AttestState | null }>;

const W = 720, H = 360;

export function AttestChart({ agent }: Props) {
  if (!agent) return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start attesting.</div>;
  const cps = agent.checkpoints.slice(-10).reverse();
  if (cps.length === 0) return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No checkpoints yet — wait for the next interval or click Publish now.</div>;

  return (
    <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 380 }}>
      <rect x={0} y={0} width={W} height={H} fill="transparent" />
      <text x={10} y={20} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">
        recent checkpoints · newest first · {agent.totalPublished} published · {agent.totalFailed} failed
      </text>
      {cps.map((cp, i) => {
        const y = 32 + i * 30;
        return (
          <g key={cp.seq}>
            <rect x={10} y={y} width={W - 20} height={26} rx={4} fill="var(--bg-card)" stroke="var(--border)" strokeWidth={0.6} />
            <rect x={10} y={y} width={4} height={26} fill="var(--pos)" />
            <text x={22} y={y + 12} fill="var(--text)" fontSize={11} fontFamily="ui-monospace, monospace">cp #{cp.seq} · head_seq {cp.headSeq} · {cp.records} recs</text>
            <text x={22} y={y + 23} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">
              head_hash {cp.headHash.slice(0, 20)}… · sig {cp.signature.slice(0, 12)}…
            </text>
            <text x={W - 14} y={y + 12} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="end">{cp.ts.slice(11, 19)}</text>
            <text x={W - 14} y={y + 23} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="end">party {cp.party}</text>
          </g>
        );
      })}
    </svg>
  );
}
