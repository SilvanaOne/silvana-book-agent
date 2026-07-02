"use client";

import type { StateMonitorState, OrderEvent, SettlementEvent } from "@/lib/statemonitor-engine";

type Props = Readonly<{ statemonitor: StateMonitorState | null }>;

const W = 720, H = 300, PAD_L = 110, PAD_R = 20, PAD_T = 14, PAD_B = 26;

// Rows: 6 event categories, top → bottom.
const ROWS: Array<{ key: string; label: string; color: string; group: "order" | "settlement" }> = [
  { key: "created",    label: "order.created",       color: "#8aa4ff", group: "order" },
  { key: "filled",     label: "order.filled",        color: "#3ddc84", group: "order" },
  { key: "cancelled",  label: "order.cancelled",     color: "#ff5c5c", group: "order" },
  { key: "proposal",   label: "settlement.proposal", color: "#f4a742", group: "settlement" },
  { key: "settled",    label: "settlement.settled",  color: "#4aa3ff", group: "settlement" },
  { key: "failed",     label: "settlement.failed",   color: "#e05cff", group: "settlement" },
];

export function StateMonitorChart({ statemonitor }: Props) {
  if (!statemonitor) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start State Monitor to see the event timeline.</div>;
  }

  const orderEvents = statemonitor.orderEvents;
  const settlementEvents = statemonitor.settlementEvents;
  const total = orderEvents.length + settlementEvents.length;

  if (total === 0) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>Streams open — awaiting the first event…</div>;
  }

  // Time window: last minute up to now (or the earliest event, whichever is older).
  const now = Date.now();
  const earliest = Math.min(
    ...orderEvents.map((e) => e.t),
    ...settlementEvents.map((e) => e.t),
    now,
  );
  const tMin = Math.min(earliest, now - 60_000);
  const tMax = now;
  const tRange = tMax - tMin || 1;
  const x = (t: number) => PAD_L + Math.min(1, Math.max(0, (t - tMin) / tRange)) * (W - PAD_L - PAD_R);

  const rowH = (H - PAD_T - PAD_B) / ROWS.length;
  const y = (rowIdx: number) => PAD_T + rowH * (rowIdx + 0.5);

  const points: Array<{ x: number; y: number; color: string; title: string }> = [];
  for (const e of orderEvents) {
    const rowIdx = ROWS.findIndex((r) => r.key === e.kind);
    if (rowIdx < 0) continue;
    points.push({
      x: x(e.t),
      y: y(rowIdx),
      color: ROWS[rowIdx].color,
      title: `ORDER #${e.seq} ${e.kind} ${e.market} ${e.side} ${e.qty}@${e.price} id=${e.orderId}`,
    });
  }
  for (const e of settlementEvents) {
    const rowIdx = ROWS.findIndex((r) => r.key === e.kind);
    if (rowIdx < 0) continue;
    points.push({
      x: x(e.t),
      y: y(rowIdx),
      color: ROWS[rowIdx].color,
      title: `SETTLEMENT #${e.seq} ${e.kind} ${e.market} notional=${e.notional} id=${e.proposalId}`,
    });
  }

  const secondsAgoTicks = [60, 45, 30, 15, 0];

  return (
    <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 340 }}>
      <rect x={0} y={0} width={W} height={H} fill="transparent" />

      {/* Row lanes */}
      {ROWS.map((r, i) => (
        <g key={r.key}>
          <line x1={PAD_L} x2={W - PAD_R} y1={y(i)} y2={y(i)} stroke="#22222c" strokeWidth={1} />
          <circle cx={PAD_L - 12} cy={y(i)} r={4} fill={r.color} opacity={0.8} />
          <text x={PAD_L - 22} y={y(i) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{r.label}</text>
        </g>
      ))}

      {/* X-axis time ticks */}
      {secondsAgoTicks.map((s) => {
        const t = now - s * 1000;
        const xp = x(t);
        return (
          <g key={s}>
            <line x1={xp} x2={xp} y1={PAD_T} y2={H - PAD_B} stroke="#1a1a22" strokeWidth={1} strokeDasharray="2,3" />
            <text x={xp} y={H - PAD_B + 14} fill="var(--text-faint)" fontSize={10} textAnchor="middle" fontFamily="ui-monospace, monospace">{s === 0 ? "now" : `−${s}s`}</text>
          </g>
        );
      })}

      {/* Event points */}
      {points.map((p, i) => (
        <circle key={i} cx={p.x} cy={p.y} r={4.5} fill={p.color} stroke={p.color} strokeWidth={1} opacity={0.92}>
          <title>{p.title}</title>
        </circle>
      ))}
    </svg>
  );
}

// Re-export types so callers can be lightweight.
export type { OrderEvent, SettlementEvent };
