"use client";

import type { NotifierState, Sink } from "@/lib/notifier-engine";

type Props = Readonly<{ notifier: NotifierState | null }>;

const W = 720, H = 320;

export function NotifierChart({ notifier }: Props) {
  if (!notifier) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start Notifier to see the event → sinks pipeline.</div>;
  }

  const sinks = notifier.sinks;
  const sourceX = 90;
  const sinkX = W - 200;
  const sourceY = H / 2;

  // Layout: sinks stacked vertically at right
  const sinkRowH = sinks.length > 0 ? Math.min(60, (H - 40) / sinks.length) : 0;
  const sinksTop = (H - sinkRowH * sinks.length) / 2;

  // Bar chart on far right — delivered vs failed per sink.
  const maxCount = Math.max(1, ...sinks.map((s) => s.deliveredCount + s.failedCount));

  return (
    <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 360 }}>
      <rect x={0} y={0} width={W} height={H} fill="transparent" />

      {/* Source node */}
      <g>
        <circle cx={sourceX} cy={sourceY} r={36} fill="var(--bg-card)" stroke="var(--accent)" strokeWidth={1.6} />
        <text x={sourceX} y={sourceY - 4} fill="var(--accent)" fontSize={12} textAnchor="middle" fontFamily="ui-monospace, monospace">events</text>
        <text x={sourceX} y={sourceY + 12} fill="var(--text-faint)" fontSize={10} textAnchor="middle" fontFamily="ui-monospace, monospace">{notifier.totalEvents}</text>
        <text x={sourceX} y={sourceY + 58} fill="var(--text-faint)" fontSize={10} textAnchor="middle" fontFamily="ui-monospace, monospace">
          {[notifier.config.orders ? "O" : null, notifier.config.settlements ? "S" : null, notifier.config.prices ? "P" : null].filter(Boolean).join(" · ")}
        </text>
      </g>

      {/* Sinks */}
      {sinks.map((s: Sink, i: number) => {
        const y = sinksTop + sinkRowH * i + sinkRowH / 2;
        const total = s.deliveredCount + s.failedCount;
        const failPct = total > 0 ? s.failedCount / total : 0;
        // Pipe color pulses toward red when failure rate spikes on webhook.
        const stroke = s.kind === "webhook"
          ? (failPct > 0.15 ? "var(--neg)" : "var(--accent)")
          : "var(--pos)";
        const barW = 90;
        const deliveredW = (s.deliveredCount / maxCount) * barW;
        const failedW = (s.failedCount / maxCount) * barW;

        return (
          <g key={s.label}>
            {/* Pipe from source to sink */}
            <path
              d={`M ${sourceX + 36} ${sourceY} C ${(sourceX + sinkX) / 2} ${sourceY}, ${(sourceX + sinkX) / 2} ${y}, ${sinkX - 6} ${y}`}
              stroke={stroke}
              strokeWidth={1.4}
              fill="none"
              opacity={0.75}
            />
            {/* Sink node */}
            <rect x={sinkX} y={y - 18} width={140} height={36} rx={6} fill="var(--bg-card)" stroke={stroke} strokeWidth={1.4} />
            <text x={sinkX + 8} y={y - 4} fill="var(--text-main, #ececf1)" fontSize={11} fontFamily="ui-monospace, monospace">
              {truncate(s.label, 18)}
            </text>
            <text x={sinkX + 8} y={y + 10} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">
              {s.kind}
            </text>

            {/* Right-side bar chart delivered / failed */}
            <rect x={sinkX + 148} y={y - 6} width={deliveredW} height={5} fill="var(--pos)" />
            <rect x={sinkX + 148} y={y + 2}  width={failedW}    height={5} fill="var(--neg)" />
            <text x={sinkX + 148 + barW + 6} y={y + 3} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">
              {s.deliveredCount}/{s.failedCount}
            </text>
          </g>
        );
      })}

      {/* Legend */}
      <g transform={`translate(20, ${H - 20})`}>
        <rect x={0} y={-8} width={10} height={5} fill="var(--pos)" />
        <text x={16} y={-3} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">delivered</text>
        <rect x={90} y={-8} width={10} height={5} fill="var(--neg)" />
        <text x={106} y={-3} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">failed</text>
      </g>
    </svg>
  );
}

function truncate(s: string, n: number): string {
  return s.length <= n ? s : s.slice(0, n - 1) + "…";
}
