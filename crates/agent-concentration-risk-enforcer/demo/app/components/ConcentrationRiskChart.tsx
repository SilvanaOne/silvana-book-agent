"use client";

import type { ConcentrationRiskState, Cancellation } from "@/lib/concentrationrisk-engine";

type Props = Readonly<{ concentrationrisk: ConcentrationRiskState | null }>;

const W = 720, H = 320, PAD_L = 120, PAD_R = 40, PAD_T = 30, PAD_B = 40;

export function ConcentrationRiskChart({ concentrationrisk }: Props) {
  if (!concentrationrisk) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>
      No data — start Concentration Risk to see per-instrument share bars, thresholds and cancel events.
    </div>;
  }

  const positions = concentrationrisk.positions;
  const { maxSharePct, minSharePct } = concentrationrisk.config;
  const n = positions.length;

  // X axis: 0–100 (percent)
  const chartW = W - PAD_L - PAD_R;
  const chartH = H - PAD_T - PAD_B;
  const x = (pct: number) => PAD_L + Math.max(0, Math.min(100, pct)) / 100 * chartW;
  const rowH = chartH / Math.max(1, n);
  const barH = Math.max(12, rowH * 0.55);

  const colorFor = (share: number): string => {
    if (share > maxSharePct) return "var(--neg)";
    if (share < minSharePct) return "var(--neg)";
    if (share > maxSharePct - 5 || share < minSharePct + 5) return "#e6a341";
    return "var(--pos)";
  };

  const maxX = x(maxSharePct);
  const minX = x(minSharePct);

  // Group recent cancellations per instrument for the marker overlay
  const cancelsByInstrument = new Map<string, Cancellation[]>();
  for (const c of concentrationrisk.cancellationsHistory) {
    const list = cancelsByInstrument.get(c.instrument) ?? [];
    list.push(c);
    cancelsByInstrument.set(c.instrument, list);
  }

  const xTicks = [0, 25, 50, 75, 100];

  return (
    <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 360 }}>
      <rect x={0} y={0} width={W} height={H} fill="transparent" />

      {/* X-axis grid */}
      {xTicks.map((v) => (
        <g key={v}>
          <line x1={x(v)} x2={x(v)} y1={PAD_T} y2={H - PAD_B} stroke="#22222c" strokeWidth={1} />
          <text x={x(v)} y={H - PAD_B + 14} fill="var(--text-faint)" fontSize={10} textAnchor="middle" fontFamily="ui-monospace, monospace">{v}%</text>
        </g>
      ))}

      {/* Threshold lines */}
      <line x1={maxX} x2={maxX} y1={PAD_T} y2={H - PAD_B} stroke="var(--neg)" strokeWidth={1.5} strokeDasharray="4,3" opacity={0.9} />
      <text x={maxX} y={PAD_T - 6} fill="var(--neg)" fontSize={10} textAnchor="middle" fontFamily="ui-monospace, monospace">max {maxSharePct}%</text>
      <line x1={minX} x2={minX} y1={PAD_T} y2={H - PAD_B} stroke="var(--neg)" strokeWidth={1.5} strokeDasharray="4,3" opacity={0.75} />
      <text x={minX} y={PAD_T - 6} fill="var(--neg)" fontSize={10} textAnchor="middle" fontFamily="ui-monospace, monospace">min {minSharePct}%</text>

      {/* Bars */}
      {positions.map((p, i) => {
        const share = p.sharePct ?? 0;
        const yTop = PAD_T + i * rowH + (rowH - barH) / 2;
        const yMid = yTop + barH / 2;
        const c = colorFor(share);
        const cancels = cancelsByInstrument.get(p.name) ?? [];
        return (
          <g key={p.name}>
            {/* row background */}
            <rect x={PAD_L} y={yTop} width={chartW} height={barH} fill="#181820" rx={3} />
            {/* bar */}
            <rect x={PAD_L} y={yTop} width={x(share) - PAD_L} height={barH} fill={c} opacity={0.85} rx={3} />
            {/* label left */}
            <text x={PAD_L - 8} y={yMid + 4} fill="var(--text)" fontSize={11} textAnchor="end" fontFamily="ui-monospace, monospace">{p.name}</text>
            <text x={PAD_L - 8} y={yMid + 16} fill="var(--text-faint)" fontSize={9} textAnchor="end" fontFamily="ui-monospace, monospace">{p.market}</text>
            {/* share label right */}
            <text x={x(share) + 6} y={yMid + 4} fill={c} fontSize={11} fontFamily="ui-monospace, monospace">{share.toFixed(1)}%</text>
            {/* cancel crosses at bar end */}
            {cancels.slice(-6).map((cc, idx) => {
              const cx = x(cc.sharePct);
              const off = (idx - cancels.slice(-6).length / 2) * 10;
              return (
                <g key={cc.seq} transform={`translate(${cx + off}, ${yMid})`}>
                  <line x1={-4} x2={4} y1={-4} y2={4} stroke="#ff5c5c" strokeWidth={1.5} />
                  <line x1={-4} x2={4} y1={4} y2={-4} stroke="#ff5c5c" strokeWidth={1.5} />
                  <title>{`#${cc.seq} ${cc.side} on ${cc.market} — share ${cc.sharePct.toFixed(2)}% ${cc.reason === "over-max" ? ">" : "<"} ${cc.thresholdPct}%${cc.dryRun ? " (dry-run)" : ""}`}</title>
                </g>
              );
            })}
          </g>
        );
      })}

      {/* Title / footer */}
      <text x={PAD_L} y={PAD_T - 12} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">share of portfolio (quote value)</text>
      <text x={W - PAD_R} y={H - 6} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">
        portfolio = {concentrationrisk.totalPortfolio.toFixed(2)} · price = {concentrationrisk.currentPrice.toFixed(4)}
      </text>
    </svg>
  );
}
