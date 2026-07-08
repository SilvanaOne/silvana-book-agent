"use client";

import type { GenesisState } from "@/lib/genesis-engine";

type Props = Readonly<{ agent: GenesisState | null }>;
const W = 720, H = 400;

export function GenesisChart({ agent }: Props) {
  if (!agent || agent.steps.length === 0) return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>Send a spec to compile — try &quot;buy 100 CC-USDC over 1 hour using TWAP&quot;</div>;
  const step = agent.steps[agent.steps.length - 1];
  const color = step.error ? "var(--neg)" : "var(--pos)";
  const tomlLines = step.toml.split("\n");

  return (
    <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 420 }}>
      <rect x={0} y={0} width={W} height={H} fill="transparent" />

      <text x={10} y={20} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">
        latest compile #{step.seq} · {step.algorithm}
      </text>

      {/* Spec box */}
      <rect x={10} y={30} width={W - 20} height={40} rx={4} fill="var(--bg-card)" stroke="var(--border)" strokeWidth={0.6} />
      <text x={20} y={46} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">SPEC (natural language)</text>
      <text x={20} y={62} fill="var(--text)" fontSize={12} fontFamily="ui-monospace, monospace">{truncate(step.spec, 84)}</text>

      {/* Arrow */}
      <text x={W / 2} y={92} fill="var(--accent)" fontSize={16} textAnchor="middle">↓</text>

      {/* Parsed pills */}
      <g transform="translate(10, 100)">
        {pill(0, "market", step.market, "var(--accent)")}
        {pill(120, "side", step.side, step.side === "buy" ? "var(--pos)" : "var(--neg)")}
        {pill(220, "algo", step.algorithm, "var(--accent)")}
        {pill(370, "total", step.total, "var(--pos)")}
      </g>

      {/* TOML output */}
      <rect x={10} y={150} width={W - 20} height={H - 170} rx={4} fill="var(--bg-card)" stroke={color} strokeWidth={1} />
      <text x={20} y={166} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">GENERATED (drop into agent-algo-order --plan)</text>
      {tomlLines.slice(0, 14).map((line, i) => (
        <text key={i} x={20} y={186 + i * 14} fill={step.error ? "var(--neg)" : "var(--text)"} fontSize={11} fontFamily="ui-monospace, monospace">{line || " "}</text>
      ))}
      {step.error && (
        <text x={W - 20} y={166} fill="var(--neg)" fontSize={11} fontFamily="ui-monospace, monospace" textAnchor="end">ERROR: {truncate(step.error, 44)}</text>
      )}
    </svg>
  );
}

function pill(x: number, label: string, value: string, color: string) {
  return (
    <g transform={`translate(${x}, 0)`}>
      <rect x={0} y={0} width={100} height={34} rx={5} fill="var(--bg-card)" stroke="var(--border)" strokeWidth={0.6} />
      <text x={8} y={13} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">{label}</text>
      <text x={8} y={28} fill={color} fontSize={12} fontFamily="ui-monospace, monospace">{truncate(value, 12)}</text>
    </g>
  );
}

function truncate(s: string, n: number) { return s.length <= n ? s : s.slice(0, n - 1) + "…"; }
