"use client";

import type { Tick } from "@/lib/store";
import type { FailureRecoveryState } from "@/lib/failurerecovery-engine";

type Props = Readonly<{ ticks: readonly Tick[]; failurerecovery: FailureRecoveryState | null }>;

const W = 720, H = 320, PAD_L = 90, PAD_R = 20, PAD_T = 14, PAD_B = 28;

export function FailureRecoveryChart({ ticks, failurerecovery }: Props) {
  if (!failurerecovery || ticks.length === 0) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start Failure Recovery to see proposal lifetimes and sweep events.</div>;
  }
  const now = ticks[ticks.length - 1].t;
  // Show up to 20 most recent proposals.
  const recent = failurerecovery.proposals.slice(-20);
  if (recent.length === 0) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>Watchdog active — waiting for proposals to arrive…</div>;
  }
  const tMin = Math.min(...recent.map((p) => p.createdAt));
  const tMax = now;
  const tRange = Math.max(tMax - tMin, 1);

  const x = (t: number) => PAD_L + ((t - tMin) / tRange) * (W - PAD_L - PAD_R);
  const rowH = (H - PAD_T - PAD_B) / Math.max(recent.length, 1);
  const y = (i: number) => PAD_T + i * rowH + rowH / 2;
  const barH = Math.max(6, rowH * 0.6);

  // maxPendingAgeSecs dashed vertical: this many ms after each row's createdAt.
  const stalenessMs = failurerecovery.config.maxPendingAgeSecs * 1000;

  const colorFor = (status: string): string => {
    if (status === "failed") return "var(--neg)";
    if (status === "settled") return "var(--pos)";
    return "var(--accent)"; // pending
  };

  return (
    <svg viewBox={`0 0 ${W + 40} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 360 }}>
      <rect x={0} y={0} width={W + 40} height={H} fill="transparent" />

      {/* horizontal grid + row labels */}
      {recent.map((p, i) => (
        <g key={`row-${p.id}`}>
          <line x1={PAD_L} x2={W - PAD_R} y1={y(i)} y2={y(i)} stroke="#22222c" strokeWidth={0.5} opacity={0.5} />
          <text x={PAD_L - 8} y={y(i) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">#{p.id}</text>
        </g>
      ))}

      {/* proposal bars */}
      {recent.map((p, i) => {
        const start = p.createdAt;
        const end = p.resolvedAt ?? now;
        const xs = x(start);
        const xe = x(end);
        const barY = y(i) - barH / 2;
        const col = colorFor(p.status);
        const staleX = x(Math.min(start + stalenessMs, tMax));
        const showStaleMarker = start + stalenessMs <= tMax;
        return (
          <g key={`bar-${p.id}`}>
            <rect x={xs} y={barY} width={Math.max(2, xe - xs)} height={barH} fill={col} opacity={0.55} rx={2} />
            {showStaleMarker && (
              <line x1={staleX} x2={staleX} y1={barY - 2} y2={barY + barH + 2} stroke="#f2b263" strokeWidth={1} strokeDasharray="2,2" />
            )}
            {/* X marker for cancel-sweep on this proposal (when staleFlagged and cancels happened) */}
            {p.staleFlagged && failurerecovery.lastSweepAt && failurerecovery.config.cancelRelatedOrders && !failurerecovery.config.dryRun && (
              <g>
                <line x1={x(failurerecovery.lastSweepAt) - 4} y1={y(i) - 4} x2={x(failurerecovery.lastSweepAt) + 4} y2={y(i) + 4} stroke="var(--neg)" strokeWidth={1.6} />
                <line x1={x(failurerecovery.lastSweepAt) - 4} y1={y(i) + 4} x2={x(failurerecovery.lastSweepAt) + 4} y2={y(i) - 4} stroke="var(--neg)" strokeWidth={1.6} />
              </g>
            )}
          </g>
        );
      })}

      {/* time axis (now) */}
      <line x1={x(tMax)} x2={x(tMax)} y1={PAD_T} y2={H - PAD_B} stroke="#ececf1" strokeWidth={0.7} opacity={0.6} />
      <text x={x(tMax)} y={H - PAD_B + 14} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">now</text>
      <text x={PAD_L} y={H - PAD_B + 14} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">
        window {Math.round(tRange / 1000)}s · pending=<tspan fill="var(--accent)">■</tspan> settled=<tspan fill="var(--pos)">■</tspan> failed=<tspan fill="var(--neg)">■</tspan> · <tspan fill="#f2b263">┊</tspan> stale threshold
      </text>
    </svg>
  );
}
