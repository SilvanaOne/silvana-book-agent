"use client";

import type { ExplainState } from "@/lib/explain-engine";

type Props = Readonly<{ agent: ExplainState | null }>;
const W = 720, H = 400;

export function ExplainChart({ agent }: Props) {
  if (!agent || agent.explanations.length === 0) return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>Load a history to see explanations.</div>;
  const last = agent.explanations[agent.explanations.length - 1];
  const side = last.side === "BID" ? "var(--pos)" : "var(--neg)";
  const kindColor = last.forKind === "order.created" ? "var(--accent)" : "var(--pos)";

  return (
    <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 420 }}>
      <rect x={0} y={0} width={W} height={H} fill="transparent" />

      <text x={10} y={20} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">
        latest explanation · {agent.totalExplained} total  · model {agent.config.model}
      </text>

      {/* Header */}
      <g transform="translate(10, 32)">
        <rect x={0} y={0} width={W - 20} height={30} rx={4} fill="var(--bg-card)" stroke="var(--border)" strokeWidth={0.6} />
        <text x={10} y={12} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">EVENT</text>
        <text x={10} y={26} fill="var(--text)" fontSize={11} fontFamily="ui-monospace, monospace">
          <tspan fill={kindColor}>{last.forKind}</tspan> · seq={last.forSeq} · {last.market} · <tspan fill={side}>{last.side}</tspan> · qty={last.quantity.toFixed(2)} · @{last.price.toFixed(4)} · ntl {last.notional.toFixed(2)}
        </text>
      </g>

      {/* Rationale */}
      <g transform="translate(10, 74)">
        <rect x={0} y={0} width={W - 20} height={140} rx={4} fill="var(--bg-card)" stroke="var(--pos)" strokeWidth={1} />
        <text x={10} y={14} fill="var(--pos)" fontSize={11} fontFamily="ui-monospace, monospace">RATIONALE</text>
        {wrapLines(last.rationale, 90).slice(0, 6).map((line, i) => (
          <text key={i} x={10} y={30 + i * 15} fill="var(--text)" fontSize={11} fontFamily="ui-monospace, monospace">{line}</text>
        ))}
      </g>

      {/* Counterfactual */}
      <g transform={`translate(10, ${74 + 150})`}>
        <rect x={0} y={0} width={W - 20} height={140} rx={4} fill="var(--bg-card)" stroke="var(--accent)" strokeWidth={1} />
        <text x={10} y={14} fill="var(--accent)" fontSize={11} fontFamily="ui-monospace, monospace">COUNTERFACTUAL — what if we didn&apos;t?</text>
        {wrapLines(last.counterfactual, 90).slice(0, 6).map((line, i) => (
          <text key={i} x={10} y={30 + i * 15} fill="var(--text)" fontSize={11} fontFamily="ui-monospace, monospace">{line}</text>
        ))}
      </g>
    </svg>
  );
}

function wrapLines(text: string, width: number): string[] {
  const words = text.split(/\s+/);
  const lines: string[] = [];
  let cur = "";
  for (const w of words) {
    if ((cur + " " + w).trim().length > width) {
      if (cur) lines.push(cur.trim());
      cur = w;
    } else {
      cur = (cur + " " + w).trim();
    }
  }
  if (cur) lines.push(cur.trim());
  return lines;
}
