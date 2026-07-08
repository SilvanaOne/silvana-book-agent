"use client";

import { classLabelColor, type ClassLabel, type ClassifierState } from "@/lib/classifier-engine";

type Props = Readonly<{ agent: ClassifierState | null }>;
const W = 720, H = 380;

export function ClassifierChart({ agent }: Props) {
  if (!agent) return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start classifying.</div>;
  const recent = agent.anomalies.slice(-10).reverse();
  const classes: ClassLabel[] = ["spoofing", "wash_trading", "stuck_settlement", "normal_volatility"];
  const maxCount = Math.max(1, ...classes.map((c) => agent.byClass[c] ?? 0));

  return (
    <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 400 }}>
      <rect x={0} y={0} width={W} height={H} fill="transparent" />

      <text x={10} y={20} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">recent classifications</text>
      {recent.length === 0 && <text x={10} y={40} fill="var(--text-faint)" fontSize={11} fontFamily="ui-monospace, monospace">Waiting for anomalies…</text>}
      {recent.map((a, i) => {
        const y = 30 + i * 20;
        const color = classLabelColor(a.classLabel);
        return (
          <g key={a.seq}>
            <rect x={10} y={y} width={140} height={16} rx={3} fill="var(--bg-card)" stroke={color} strokeWidth={1} />
            <text x={80} y={y + 12} fill={color} fontSize={9} fontFamily="ui-monospace, monospace" textAnchor="middle">{a.classLabel.toUpperCase().slice(0, 18)}</text>
            <text x={160} y={y + 12} fill="var(--text)" fontSize={10} fontFamily="ui-monospace, monospace">
              #{a.seq} {a.kind.replace("anomaly.", "")} · {a.market} · win {a.windowSize}
            </text>
            <text x={W - 12} y={y + 12} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="end">{truncate(a.detail, 32)}</text>
          </g>
        );
      })}

      <text x={10} y={H - 88} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">counts by class</text>
      {classes.map((cls, i) => {
        const y = H - 76 + i * 14;
        const n = agent.byClass[cls] ?? 0;
        const w = (n / maxCount) * (W - 260);
        return (
          <g key={cls}>
            <text x={10} y={y + 8} fill="var(--text)" fontSize={11} fontFamily="ui-monospace, monospace">{cls}</text>
            <rect x={200} y={y} width={w} height={9} fill={classLabelColor(cls)} opacity={0.85} />
            <text x={200 + w + 6} y={y + 8} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">{n}</text>
          </g>
        );
      })}
    </svg>
  );
}

function truncate(s: string, n: number) { return s.length <= n ? s : s.slice(0, n - 1) + "…"; }
