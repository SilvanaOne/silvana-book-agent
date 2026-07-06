"use client";

import type { EvalRecord, LegalState } from "@/lib/legal-engine";

type Props = Readonly<{ agent: LegalState | null }>;

const W = 720, H = 360;

export function LegalChart({ agent }: Props) {
  if (!agent) return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start evaluating.</div>;
  const evals = agent.recentEvals.slice(-10).reverse();
  const jx = Object.entries(agent.perJurisdiction).slice(0, 6);
  const maxTotal = Math.max(1, ...jx.map(([, b]) => b.total));

  return (
    <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 380 }}>
      <rect x={0} y={0} width={W} height={H} fill="transparent" />

      {/* top: recent verdicts */}
      <text x={10} y={20} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">recent verdicts</text>
      {evals.length === 0 && <text x={10} y={40} fill="var(--text-faint)" fontSize={11} fontFamily="ui-monospace, monospace">No verdicts yet…</text>}
      {evals.map((r: EvalRecord, i: number) => {
        const y = 30 + i * 20;
        const color = r.verdict === "legal" ? "var(--pos)" : "var(--neg)";
        return (
          <g key={r.seq}>
            <rect x={10} y={y} width={54} height={16} rx={3} fill="var(--bg-card)" stroke={color} strokeWidth={1} />
            <text x={37} y={y + 12} fill={color} fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="middle">{r.verdict.toUpperCase()}</text>
            <text x={72} y={y + 12} fill="var(--text)" fontSize={10} fontFamily="ui-monospace, monospace">
              {truncate(r.settlement.buyer, 14)}({r.settlement.buyerJx ?? "?"}) → {truncate(r.settlement.seller, 14)}({r.settlement.sellerJx ?? "?"}) · {r.settlement.market} · {r.settlement.notional.toFixed(0)}
            </text>
            {r.hits.length > 0 && (
              <text x={W - 10} y={y + 12} fill="var(--neg)" fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="end">{truncate(r.hits.join(","), 30)}</text>
            )}
          </g>
        );
      })}

      {/* bottom: per-jurisdiction bars */}
      <text x={10} y={H - 118} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">activity per jurisdiction</text>
      {jx.length === 0 ? (
        <text x={10} y={H - 96} fill="var(--text-faint)" fontSize={11} fontFamily="ui-monospace, monospace">No jurisdiction activity yet…</text>
      ) : jx.map(([j, b], i) => {
        const y = H - 104 + i * 14;
        const w = (b.total / maxTotal) * (W - 260);
        const violW = (b.violations / maxTotal) * (W - 260);
        return (
          <g key={j}>
            <text x={10} y={y + 8} fill="var(--text)" fontSize={11} fontFamily="ui-monospace, monospace">{j}</text>
            <rect x={80} y={y + 2} width={w} height={8} fill="var(--pos)" opacity={0.5} />
            <rect x={80} y={y + 2} width={violW} height={8} fill="var(--neg)" />
            <text x={80 + w + 6} y={y + 9} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">{b.total} tot · {b.violations} viol</text>
          </g>
        );
      })}
    </svg>
  );
}

function truncate(s: string, n: number): string { return s.length <= n ? s : s.slice(0, n - 1) + "…"; }
