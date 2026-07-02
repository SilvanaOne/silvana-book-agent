"use client";

import type { WitnessesState, CommandInvocation } from "@/lib/witnesses-engine";

type Props = Readonly<{ witnesses: WitnessesState | null }>;

// Consistent colour per kind so the timeline is scannable.
const KIND_COLORS: Readonly<Record<string, string>> = {
  "settlement.settled": "#6de6a3",
  "settlement.failed": "#ff8dab",
  "settlement.cancelled": "#f6c177",
  "settlement.proposal_created": "#9ec5ff",
  "order.filled": "#a78bfa",
  "order.cancelled": "#c0c0d0",
};

function kindColor(k: string): string {
  return KIND_COLORS[k] ?? "#8080a0";
}

function fmtTime(t: number): string {
  const d = new Date(t);
  const pad = (n: number) => n.toString().padStart(2, "0");
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

export function WitnessesChart({ witnesses }: Props) {
  if (!witnesses || witnesses.invocations.length === 0) {
    return (
      <div style={{ height: 300, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>
        No invocations yet — waiting for a matching event.
      </div>
    );
  }

  const inv = witnesses.invocations;
  const W = 720, H = 90, PAD_L = 12, PAD_R = 12, PAD_T = 24, PAD_B = 24;
  const tMin = inv[0].t;
  const tMax = inv[inv.length - 1].t;
  const tRange = Math.max(1, tMax - tMin);
  const x = (t: number) => PAD_L + ((t - tMin) / tRange) * (W - PAD_L - PAD_R);

  // Legend: kinds actually invoked.
  const kindsInFlight = Array.from(new Set(inv.map((i) => i.eventKind)));

  const recent = [...inv].reverse().slice(0, 15);

  return (
    <div className="stack">
      {/* Timeline */}
      <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 110 }}>
        <line x1={PAD_L} x2={W - PAD_R} y1={H / 2} y2={H / 2} stroke="#22222c" strokeWidth={1} />
        {inv.map((i) => (
          <g key={i.seq}>
            <circle
              cx={x(i.t)}
              cy={H / 2}
              r={5}
              fill={i.exitCode === 0 ? kindColor(i.eventKind) : "transparent"}
              stroke={kindColor(i.eventKind)}
              strokeWidth={1.6}
              opacity={0.95}
            />
            {i.exitCode !== 0 && (
              <text x={x(i.t)} y={H / 2 + 3} textAnchor="middle" fill="#ff8dab" fontSize={9} fontWeight={700}>×</text>
            )}
          </g>
        ))}
        <text x={PAD_L} y={H - 6} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">{fmtTime(tMin)}</text>
        <text x={W - PAD_R} y={H - 6} textAnchor="end" fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">{fmtTime(tMax)}</text>
      </svg>

      {/* Legend */}
      <div className="row" style={{ flexWrap: "wrap", gap: 10, fontSize: 11 }}>
        {kindsInFlight.map((k) => (
          <span key={k} className="mono" style={{ display: "inline-flex", alignItems: "center", gap: 4 }}>
            <span style={{ display: "inline-block", width: 10, height: 10, borderRadius: "50%", background: kindColor(k) }} />
            {k}
          </span>
        ))}
      </div>

      {/* Recent invocations table */}
      <div className="mono" style={{ fontSize: 11, marginTop: 8 }}>
        <div className="row" style={{ display: "grid", gridTemplateColumns: "70px 1fr 1fr 55px 70px", gap: 6, padding: "4px 0", borderBottom: "1px solid #22222c", color: "var(--text-faint)" }}>
          <span>time</span><span>event</span><span>command</span><span>exit</span><span>duration</span>
        </div>
        {recent.map((i: CommandInvocation) => (
          <div key={i.seq} className="row" style={{ display: "grid", gridTemplateColumns: "70px 1fr 1fr 55px 70px", gap: 6, padding: "3px 0", borderBottom: "1px solid #1c1c26" }}>
            <span className="muted">{fmtTime(i.t)}</span>
            <span style={{ color: kindColor(i.eventKind) }}>{i.eventKind}</span>
            <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }} title={i.command}>{i.command}</span>
            <span className={i.exitCode === 0 ? "positive" : "negative"}>{i.exitCode === 0 ? "✓ 0" : `✗ ${i.exitCode}`}</span>
            <span className="muted">{i.durationMs}ms</span>
          </div>
        ))}
      </div>
    </div>
  );
}
