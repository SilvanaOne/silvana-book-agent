"use client";

import type { NewsState } from "@/lib/news-engine";

type Props = Readonly<{ agent: NewsState | null }>;
const W = 720, H = 380;

export function NewsChart({ agent }: Props) {
  if (!agent) return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start scoring.</div>;
  const items = agent.headlines.slice(-8).reverse();
  if (items.length === 0) return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>Waiting for first headline…</div>;

  const thr = agent.config.threshold;

  return (
    <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 400 }}>
      <rect x={0} y={0} width={W} height={H} fill="transparent" />

      <text x={10} y={20} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">
        recent headlines · threshold ±{thr.toFixed(2)}
      </text>

      {items.map((h, i) => {
        const y = 32 + i * 42;
        const color = h.emitted ? (h.side === "buy" ? "var(--pos)" : "var(--neg)") : "var(--text-faint)";

        // Score bar centered at midX
        const midX = 260;
        const barMax = 200;
        const w = Math.abs(h.score) * barMax;
        const barX = h.score >= 0 ? midX : midX - w;

        return (
          <g key={h.seq}>
            <rect x={10} y={y} width={W - 20} height={38} rx={4} fill="var(--bg-card)" stroke="var(--border)" strokeWidth={0.6} />
            <rect x={10} y={y} width={4} height={38} fill={color} />
            <text x={20} y={y + 14} fill="var(--text)" fontSize={11} fontFamily="ui-monospace, monospace">#{h.seq} {truncate(h.text, 76)}</text>
            <text x={20} y={y + 30} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">
              {h.emitted ? `EMIT ${h.side ?? ""} → ${h.matchedMarkets.join(", ")}` : `SKIP ${h.skipReason ?? ""}`}
            </text>
            {/* Score bar */}
            <line x1={midX} x2={midX} y1={y + 4} y2={y + 34} stroke="var(--border)" strokeWidth={0.5} />
            <rect x={barX} y={y + 16} width={w} height={10} fill={color} opacity={0.85} />
            {/* threshold ticks */}
            <line x1={midX + thr * barMax} x2={midX + thr * barMax} y1={y + 12} y2={y + 30} stroke="var(--accent)" strokeWidth={0.6} strokeDasharray="2 2" />
            <line x1={midX - thr * barMax} x2={midX - thr * barMax} y1={y + 12} y2={y + 30} stroke="var(--accent)" strokeWidth={0.6} strokeDasharray="2 2" />
            <text x={W - 12} y={y + 24} fill={color} fontSize={11} fontFamily="ui-monospace, monospace" textAnchor="end">
              {h.score >= 0 ? "+" : ""}{h.score.toFixed(2)}
            </text>
          </g>
        );
      })}
    </svg>
  );
}

function truncate(s: string, n: number) { return s.length <= n ? s : s.slice(0, n - 1) + "…"; }
