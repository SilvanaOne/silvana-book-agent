"use client";

import type { ObStreamState } from "@/lib/obstream-engine";

type Props = Readonly<{ agent: ObStreamState | null }>;

const W = 720, H = 340;

export function ObStreamChart({ agent }: Props) {
  if (!agent) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start orderbook-streaming to see the depth ladder + sink fan-out.</div>;
  }

  const primary = agent.books[0];
  if (!primary) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No markets configured.</div>;
  }

  const ladderW = 380;
  const gapX = 30;
  const sinksX = ladderW + gapX + 20;
  const sinksW = W - sinksX - 10;

  const maxQty = Math.max(1, ...primary.bids.map((l) => l.qty), ...primary.offers.map((l) => l.qty));
  const rowH = Math.min(18, (H - 60) / (primary.bids.length + primary.offers.length + 1));
  const midY = 40 + primary.offers.length * rowH;

  // Sinks list: total delivered/failed bar per sink.
  const sinks = agent.sinks;
  const sinkRowH = sinks.length > 0 ? Math.min(46, (H - 60) / Math.max(sinks.length, 1)) : 0;
  const sinksTop = 40 + (H - 40 - sinkRowH * sinks.length) / 2;
  const maxCount = Math.max(1, ...sinks.map((s) => s.deliveredCount + s.failedCount));

  return (
    <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 380 }}>
      <rect x={0} y={0} width={W} height={H} fill="transparent" />

      {/* Left side: depth ladder for the primary market */}
      <text x={10} y={22} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">{primary.market} · depth ladder (top {primary.offers.length} × {primary.bids.length})</text>

      {/* Offer levels — descending price from top */}
      {primary.offers.slice().reverse().map((lvl, i) => {
        const y = 40 + i * rowH;
        const barW = (lvl.qty / maxQty) * (ladderW / 2 - 60);
        return (
          <g key={`ask-${i}`}>
            <text x={10} y={y + rowH - 4} fill="var(--neg)" fontSize={10} fontFamily="ui-monospace, monospace">{fmt(lvl.price)}</text>
            <rect x={90} y={y + 2} width={barW} height={rowH - 4} fill="rgba(239,68,68,0.35)" />
            <text x={90 + barW + 6} y={y + rowH - 4} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">{lvl.qty.toFixed(2)}</text>
          </g>
        );
      })}

      {/* Mid separator */}
      <line x1={5} x2={ladderW - 5} y1={midY} y2={midY} stroke="var(--accent)" strokeWidth={0.6} strokeDasharray="3 3" opacity={0.7} />
      <text x={ladderW - 5} y={midY - 4} fill="var(--accent)" fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="end">mid {fmt(primary.price)}</text>

      {/* Bid levels — from mid down */}
      {primary.bids.map((lvl, i) => {
        const y = midY + rowH * i + rowH * 0.2;
        const barW = (lvl.qty / maxQty) * (ladderW / 2 - 60);
        return (
          <g key={`bid-${i}`}>
            <text x={10} y={y + rowH - 4} fill="var(--pos)" fontSize={10} fontFamily="ui-monospace, monospace">{fmt(lvl.price)}</text>
            <rect x={90} y={y + 2} width={barW} height={rowH - 4} fill="rgba(34,197,94,0.35)" />
            <text x={90 + barW + 6} y={y + rowH - 4} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">{lvl.qty.toFixed(2)}</text>
          </g>
        );
      })}

      {/* Vertical separator */}
      <line x1={ladderW + gapX / 2} x2={ladderW + gapX / 2} y1={20} y2={H - 12} stroke="var(--border)" strokeWidth={0.6} />

      {/* Right side: sinks fan-out */}
      <text x={sinksX} y={22} fill="var(--accent)" fontSize={12} fontFamily="ui-monospace, monospace">sinks · {agent.totalEvents} events dispatched</text>

      {sinks.map((s, i) => {
        const y = sinksTop + sinkRowH * i;
        const total = s.deliveredCount + s.failedCount;
        const failPct = total > 0 ? s.failedCount / total : 0;
        const stroke = s.kind === "webhook"
          ? (failPct > 0.15 ? "var(--neg)" : "var(--accent)")
          : "var(--pos)";
        const barMax = sinksW - 20;
        const deliveredW = (s.deliveredCount / maxCount) * barMax;
        const failedW = (s.failedCount / maxCount) * barMax;
        return (
          <g key={s.label}>
            <rect x={sinksX} y={y} width={sinksW} height={sinkRowH - 6} rx={6} fill="var(--bg-card)" stroke={stroke} strokeWidth={1.2} />
            <text x={sinksX + 8} y={y + 14} fill="var(--text-main, #ececf1)" fontSize={11} fontFamily="ui-monospace, monospace">{truncate(s.label, 32)}</text>
            <text x={sinksX + 8} y={y + 26} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">{s.kind}</text>
            <rect x={sinksX + 8} y={y + sinkRowH - 16} width={deliveredW} height={4} fill="var(--pos)" />
            <rect x={sinksX + 8} y={y + sinkRowH - 11} width={failedW} height={4} fill="var(--neg)" />
            <text x={sinksX + sinksW - 8} y={y + sinkRowH - 8} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="end">{s.deliveredCount}/{s.failedCount}</text>
          </g>
        );
      })}

      {/* Legend */}
      <g transform={`translate(10, ${H - 6})`}>
        <rect x={0} y={-8} width={10} height={5} fill="rgba(34,197,94,0.45)" />
        <text x={16} y={-3} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">bids</text>
        <rect x={60} y={-8} width={10} height={5} fill="rgba(239,68,68,0.45)" />
        <text x={76} y={-3} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">asks</text>
        <rect x={130} y={-8} width={10} height={5} fill="var(--pos)" />
        <text x={146} y={-3} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">delivered</text>
        <rect x={220} y={-8} width={10} height={5} fill="var(--neg)" />
        <text x={236} y={-3} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">failed</text>
      </g>
    </svg>
  );
}

function truncate(s: string, n: number): string {
  return s.length <= n ? s : s.slice(0, n - 1) + "…";
}

function fmt(n: number): string {
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 6;
  return n.toFixed(digits);
}
