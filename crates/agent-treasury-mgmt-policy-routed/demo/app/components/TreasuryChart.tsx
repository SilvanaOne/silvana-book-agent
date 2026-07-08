"use client";

import type { Leg, TreasuryState } from "@/lib/treasury-engine";

type Props = Readonly<{ agent: TreasuryState | null }>;

const W = 720, H = 400;

const ROUTE_COLOR: Record<string, string> = { direct: "var(--pos)", approval: "var(--accent)", refused_cap: "var(--neg)" };

export function TreasuryChart({ agent }: Props) {
  if (!agent || !agent.lastSnapshot) return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start monitoring.</div>;
  const snap = agent.lastSnapshot;
  const legs = agent.legs.slice(-8).reverse();
  const rowH = Math.min(48, (H - 200) / Math.max(snap.rows.length, 1));

  return (
    <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 420 }}>
      <rect x={0} y={0} width={W} height={H} fill="transparent" />

      {/* Top: per-target rows */}
      <text x={10} y={20} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">
        targets · portfolio {snap.totalValue.toFixed(2)}
      </text>
      {snap.rows.map((r, i) => {
        const y = 30 + i * rowH;
        const trackX = 220, trackW = W - trackX - 20;
        const maxVal = Math.max(r.target.targetValue, r.currentValue) * 1.2;
        const targetX = trackX + (r.target.targetValue / maxVal) * trackW;
        const currentX = trackX + (r.currentValue / maxVal) * trackW;
        const color = r.breached ? "var(--neg)" : "var(--pos)";
        return (
          <g key={r.target.instrument}>
            <text x={10} y={y + 14} fill="var(--text)" fontSize={11} fontFamily="ui-monospace, monospace">{r.target.instrument}@{r.target.market}</text>
            <text x={10} y={y + 28} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">tgt {r.target.targetValue.toFixed(0)}</text>
            <rect x={trackX} y={y + 6} width={trackW} height={16} fill="var(--bg-card)" stroke="var(--border)" strokeWidth={0.6} rx={3} />
            <line x1={targetX} x2={targetX} y1={y + 3} y2={y + 25} stroke="var(--pos)" strokeWidth={1.2} strokeDasharray="3 3" />
            <line x1={currentX} x2={currentX} y1={y + 2} y2={y + 26} stroke={color} strokeWidth={2} />
            <circle cx={currentX} cy={y + 14} r={4} fill={color} />
            <text x={W - 12} y={y + 14} fill={color} fontSize={11} fontFamily="ui-monospace, monospace" textAnchor="end">
              {r.currentValue.toFixed(0)} {r.breached ? "· BREACH" : "· OK"}
            </text>
            <text x={W - 12} y={y + 28} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="end">gap {r.gap >= 0 ? "+" : ""}{r.gap.toFixed(0)}</text>
          </g>
        );
      })}

      {/* Bottom: legs */}
      <text x={10} y={H - 132} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">recent trade legs</text>
      {legs.length === 0 ? (
        <text x={10} y={H - 108} fill="var(--text-faint)" fontSize={11} fontFamily="ui-monospace, monospace">No legs yet — waiting for a breach…</text>
      ) : legs.map((leg: Leg, i: number) => {
        const y = H - 118 + i * 14;
        const color = ROUTE_COLOR[leg.route];
        return (
          <g key={leg.seq}>
            <rect x={10} y={y - 4} width={70} height={12} rx={3} fill="var(--bg-card)" stroke={color} strokeWidth={1} />
            <text x={45} y={y + 4} fill={color} fontSize={9} fontFamily="ui-monospace, monospace" textAnchor="middle">{leg.route.toUpperCase()}</text>
            <text x={88} y={y + 4} fill="var(--text)" fontSize={10} fontFamily="ui-monospace, monospace">
              {leg.side} {leg.instrument}@{leg.market} · qty {leg.qty.toFixed(4)} · {leg.notional.toFixed(2)}
            </text>
            {leg.reason && (<text x={W - 10} y={y + 4} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="end">{truncate(leg.reason, 30)}</text>)}
          </g>
        );
      })}
    </svg>
  );
}

function truncate(s: string, n: number): string { return s.length <= n ? s : s.slice(0, n - 1) + "…"; }
