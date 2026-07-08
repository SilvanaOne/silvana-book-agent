"use client";

import type { EvalRecord, ScamState } from "@/lib/scam-engine";

type Props = Readonly<{ agent: ScamState | null }>;

const W = 720, H = 360;

const SEV_COLOR: Record<string, string> = { critical: "var(--neg)", warn: "var(--accent)", info: "#ffd66b", clean: "var(--pos)" };

export function ScamChart({ agent }: Props) {
  if (!agent) return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start screening.</div>;
  const evals = agent.recentEvals.slice(-10).reverse();
  const cats = agent.config.categories;
  const maxCatHits = Math.max(1, ...Object.values(agent.perCategoryHits));

  return (
    <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 380 }}>
      <rect x={0} y={0} width={W} height={H} fill="transparent" />

      {/* left: recent evals */}
      <text x={10} y={20} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">recent verdicts</text>
      {evals.length === 0 && <text x={10} y={40} fill="var(--text-faint)" fontSize={11} fontFamily="ui-monospace, monospace">No verdicts yet…</text>}
      {evals.map((r: EvalRecord, i: number) => {
        const y = 30 + i * 20;
        const topSev = r.clean ? "clean" : r.matches.reduce<string>((acc, m) => rank(m.severity) > rank(acc) ? m.severity : acc, "info");
        const color = SEV_COLOR[topSev];
        return (
          <g key={r.seq}>
            <rect x={10} y={y} width={68} height={16} rx={3} fill="var(--bg-card)" stroke={color} strokeWidth={1} />
            <text x={44} y={y + 12} fill={color} fontSize={9} fontFamily="ui-monospace, monospace" textAnchor="middle">{topSev.toUpperCase()}</text>
            <text x={86} y={y + 12} fill="var(--text)" fontSize={10} fontFamily="ui-monospace, monospace">
              {truncate(r.settlement.buyer, 14)} → {truncate(r.settlement.seller, 14)} · {r.settlement.market} · {r.settlement.notional.toFixed(0)}
            </text>
            {r.matches.length > 0 && (
              <text x={W - 10} y={y + 12} fill={color} fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="end">
                {truncate(r.matches.map((m) => m.category).join(","), 28)}
              </text>
            )}
          </g>
        );
      })}

      {/* bottom: per-category bars */}
      <text x={10} y={H - 100} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">hits per category</text>
      {cats.map((cat, i) => {
        const y = H - 88 + i * 16;
        const hits = agent.perCategoryHits[cat.name] ?? 0;
        const w = (hits / maxCatHits) * (W - 260);
        const color = SEV_COLOR[cat.severity];
        return (
          <g key={cat.name}>
            <text x={10} y={y + 10} fill="var(--text)" fontSize={11} fontFamily="ui-monospace, monospace">{truncate(cat.name, 16)}</text>
            <text x={140} y={y + 10} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">{cat.severity}</text>
            <rect x={200} y={y + 2} width={w} height={9} fill={color} opacity={0.85} />
            <text x={200 + w + 6} y={y + 10} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">{hits} · {cat.parties.length}p</text>
          </g>
        );
      })}
    </svg>
  );
}

function rank(s: string): number { return s === "critical" ? 3 : s === "warn" ? 2 : s === "info" ? 1 : 0; }
function truncate(s: string, n: number): string { return s.length <= n ? s : s.slice(0, n - 1) + "…"; }
