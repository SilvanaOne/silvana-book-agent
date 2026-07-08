"use client";

import type { ComplianceState, EvalRecord } from "@/lib/compliance-engine";

type Props = Readonly<{ agent: ComplianceState | null }>;

const W = 720, H = 340;

export function ComplianceChart({ agent }: Props) {
  if (!agent) return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start screening.</div>;
  const evals = agent.recentEvals;
  if (evals.length === 0) return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No verdicts yet — waiting for events (accept-only mode? enable emit-accepts).</div>;

  const maxRows = 12;
  const shown = evals.slice(-maxRows).reverse();
  const rowH = Math.min(24, (H - 100) / Math.max(shown.length, 1));

  // hits-by-rule bar chart at bottom
  const hits = Object.entries(agent.hitsByRule).sort((a, b) => b[1] - a[1]).slice(0, 5);
  const maxHit = Math.max(1, ...hits.map(([, n]) => n));

  return (
    <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 360 }}>
      <rect x={0} y={0} width={W} height={H} fill="transparent" />
      <text x={10} y={20} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">recent verdicts (newest first) · accepts {agent.accepts} · rejects {agent.rejects}</text>

      {shown.map((r: EvalRecord, i: number) => {
        const y = 32 + i * rowH;
        const color = r.verdict === "accept" ? "var(--pos)" : "var(--neg)";
        return (
          <g key={r.seq}>
            <rect x={10} y={y} width={44} height={rowH - 4} rx={3} fill="var(--bg-card)" stroke={color} strokeWidth={1} />
            <text x={32} y={y + rowH / 2 + 3} fill={color} fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="middle">
              {r.verdict.toUpperCase()}
            </text>
            <text x={62} y={y + rowH / 2 + 3} fill="var(--text)" fontSize={10} fontFamily="ui-monospace, monospace">
              {r.settlement.market} · {truncate(r.settlement.buyer, 14)} → {truncate(r.settlement.seller, 14)} · {r.settlement.notional.toFixed(0)}
            </text>
            {r.hits.length > 0 && (
              <text x={W - 10} y={y + rowH / 2 + 3} fill="var(--neg)" fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="end">
                {truncate(r.hits.join(","), 30)}
              </text>
            )}
          </g>
        );
      })}

      {/* Bottom: hits-by-rule bars */}
      <text x={10} y={H - 84} fill="var(--accent)" fontSize={11} fontFamily="ui-monospace, monospace">rule hits</text>
      {hits.map(([rule, n], i) => {
        const y = H - 72 + i * 14;
        const w = (n / maxHit) * (W - 240);
        return (
          <g key={rule}>
            <text x={10} y={y + 8} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">{truncate(rule, 22)}</text>
            <rect x={140} y={y} width={w} height={9} fill="var(--neg)" opacity={0.7} />
            <text x={140 + w + 6} y={y + 8} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">{n}</text>
          </g>
        );
      })}
      {hits.length === 0 && (
        <text x={140} y={H - 60} fill="var(--pos)" fontSize={10} fontFamily="ui-monospace, monospace">no rule hits yet — all events pass</text>
      )}
    </svg>
  );
}

function truncate(s: string, n: number): string { return s.length <= n ? s : s.slice(0, n - 1) + "…"; }
