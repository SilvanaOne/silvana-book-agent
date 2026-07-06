"use client";

import type { ReplayState } from "@/lib/replay-engine";

type Props = Readonly<{ agent: ReplayState | null }>;

const W = 720, H = 380;

export function ReplayChart({ agent }: Props) {
  if (!agent) return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — load a history.</div>;
  const verdicts = agent.verdicts.slice(-10).reverse();
  const hits = Object.entries(agent.hitsByRule).sort((a, b) => b[1] - a[1]).slice(0, 6);
  const maxHit = Math.max(1, ...hits.map(([, n]) => n));

  return (
    <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 400 }}>
      <rect x={0} y={0} width={W} height={H} fill="transparent" />
      <text x={10} y={20} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">
        recent verdicts · {agent.accepts} accepts · {agent.rejects} rejects
      </text>
      {verdicts.length === 0 && <text x={10} y={40} fill="var(--text-faint)" fontSize={11} fontFamily="ui-monospace, monospace">No matching verdicts yet — try emit-accepts.</text>}
      {verdicts.map((r, i) => {
        const y = 30 + i * 20;
        const color = r.verdict === "accept" ? "var(--pos)" : "var(--neg)";
        return (
          <g key={r.seq}>
            <rect x={10} y={y} width={54} height={16} rx={3} fill="var(--bg-card)" stroke={color} strokeWidth={1} />
            <text x={37} y={y + 12} fill={color} fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="middle">{r.verdict.toUpperCase()}</text>
            <text x={72} y={y + 12} fill="var(--text)" fontSize={10} fontFamily="ui-monospace, monospace">
              seq {r.seq} · {r.eventKind} · {r.market} {r.side} · ntl {r.notional.toFixed(0)}
            </text>
            {r.hits.length > 0 && (
              <text x={W - 10} y={y + 12} fill="var(--neg)" fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="end">{truncate(r.hits.join(","), 30)}</text>
            )}
          </g>
        );
      })}

      <text x={10} y={H - 118} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">rule hits</text>
      {hits.length === 0 ? (
        <text x={10} y={H - 96} fill="var(--pos)" fontSize={11} fontFamily="ui-monospace, monospace">All events pass — loosen rules to see rejects.</text>
      ) : hits.map(([rule, n], i) => {
        const y = H - 104 + i * 16;
        const w = (n / maxHit) * (W - 260);
        return (
          <g key={rule}>
            <text x={10} y={y + 8} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">{truncate(rule, 22)}</text>
            <rect x={200} y={y} width={w} height={9} fill="var(--neg)" opacity={0.75} />
            <text x={200 + w + 6} y={y + 8} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">{n}</text>
          </g>
        );
      })}
    </svg>
  );
}

function truncate(s: string, n: number): string { return s.length <= n ? s : s.slice(0, n - 1) + "…"; }
