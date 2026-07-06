"use client";

import { anomalyColor, type AnomalyKind, type AnomalyState } from "@/lib/anomaly-engine";

type Props = Readonly<{ agent: AnomalyState | null }>;

const W = 720, H = 380;

export function AnomalyChart({ agent }: Props) {
  if (!agent) return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — load a history to scan.</div>;
  const anomalies = agent.anomalies.slice(-10).reverse();
  const kinds: AnomalyKind[] = ["stuck_settlement", "rapid_cancel", "layer_cluster", "fill_before_cancel_burst"];
  const maxCount = Math.max(1, ...kinds.map((k) => agent.byKind[k] ?? 0));

  return (
    <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 400 }}>
      <rect x={0} y={0} width={W} height={H} fill="transparent" />

      <text x={10} y={20} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">recent anomalies</text>
      {anomalies.length === 0 && <text x={10} y={40} fill="var(--pos)" fontSize={11} fontFamily="ui-monospace, monospace">No anomalies found — clean log.</text>}
      {anomalies.map((a, i) => {
        const y = 30 + i * 20;
        const color = anomalyColor(a.kind);
        return (
          <g key={`${a.seq}-${a.kind}-${i}`}>
            <rect x={10} y={y} width={140} height={16} rx={3} fill="var(--bg-card)" stroke={color} strokeWidth={1} />
            <text x={80} y={y + 12} fill={color} fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="middle">{a.kind.toUpperCase()}</text>
            <text x={160} y={y + 12} fill="var(--text)" fontSize={10} fontFamily="ui-monospace, monospace">seq {a.seq} · {a.market}</text>
            <text x={W - 10} y={y + 12} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="end">{truncate(a.detail, 44)}</text>
          </g>
        );
      })}

      <text x={10} y={H - 118} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">by kind</text>
      {kinds.map((k, i) => {
        const y = H - 104 + i * 16;
        const n = agent.byKind[k] ?? 0;
        const w = (n / maxCount) * (W - 280);
        return (
          <g key={k}>
            <text x={10} y={y + 8} fill="var(--text)" fontSize={10} fontFamily="ui-monospace, monospace">{k}</text>
            <rect x={220} y={y} width={w} height={9} fill={anomalyColor(k)} opacity={0.85} />
            <text x={220 + w + 6} y={y + 8} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">{n}</text>
          </g>
        );
      })}
    </svg>
  );
}

function truncate(s: string, n: number): string { return s.length <= n ? s : s.slice(0, n - 1) + "…"; }
