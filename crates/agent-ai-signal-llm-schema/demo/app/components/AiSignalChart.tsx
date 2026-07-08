"use client";

import type { AiSignalState } from "@/lib/ai-signal-engine";

type Props = Readonly<{ agent: AiSignalState | null }>;
const W = 720, H = 380;

export function AiSignalChart({ agent }: Props) {
  if (!agent) return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start predicting.</div>;
  const signals = agent.signals.slice(-8).reverse();

  const bidCount = agent.bySide.BID ?? 0;
  const offerCount = agent.bySide.OFFER ?? 0;
  const totalKept = bidCount + offerCount;
  const bidPct = totalKept > 0 ? bidCount / totalKept : 0.5;

  return (
    <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 400 }}>
      <rect x={0} y={0} width={W} height={H} fill="transparent" />

      <text x={10} y={20} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">
        recent AI signals · avg conf {(agent.avgConfidence * 100).toFixed(1)}%
      </text>
      {signals.length === 0 && <text x={10} y={40} fill="var(--text-faint)" fontSize={11} fontFamily="ui-monospace, monospace">Waiting for first prompt…</text>}
      {signals.map((s, i) => {
        const y = 32 + i * 36;
        const color = s.dropped ? "var(--neg)" : s.side === "BID" ? "var(--pos)" : "var(--accent)";
        const tag = s.dropped ? "DROP" : s.side;
        const confBarMax = 260;
        const confW = Math.max(2, s.confidence * confBarMax);
        return (
          <g key={s.seq}>
            <rect x={10} y={y} width={W - 20} height={32} rx={4} fill="var(--bg-card)" stroke="var(--border)" strokeWidth={0.6} />
            <rect x={10} y={y} width={4} height={32} fill={color} />
            <text x={20} y={y + 12} fill="var(--text)" fontSize={11} fontFamily="ui-monospace, monospace">#{s.seq}  {tag}  {s.market}  qty={s.quantity.toFixed(2)}  @{s.price.toFixed(4)}</text>
            <text x={20} y={y + 26} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">"{truncate(s.prompt, 66)}"</text>
            <rect x={W - confBarMax - 14} y={y + 12} width={confW} height={8} fill={color} opacity={0.85} />
            <text x={W - 14} y={y + 26} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="end">conf {(s.confidence * 100).toFixed(0)}%</text>
          </g>
        );
      })}

      {/* Bottom: BID vs OFFER pie-ish */}
      <text x={10} y={H - 40} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">direction mix (kept only)</text>
      <rect x={10} y={H - 28} width={W - 20} height={12} fill="var(--bg-card)" stroke="var(--border)" strokeWidth={0.6} rx={3} />
      <rect x={10} y={H - 28} width={(W - 20) * bidPct} height={12} fill="var(--pos)" />
      <rect x={10 + (W - 20) * bidPct} y={H - 28} width={(W - 20) * (1 - bidPct)} height={12} fill="var(--accent)" />
      <text x={10} y={H - 4} fill="var(--pos)" fontSize={10} fontFamily="ui-monospace, monospace">BID {bidCount}</text>
      <text x={W - 10} y={H - 4} fill="var(--accent)" fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="end">OFFER {offerCount}</text>
    </svg>
  );
}

function truncate(s: string, n: number) { return s.length <= n ? s : s.slice(0, n - 1) + "…"; }
