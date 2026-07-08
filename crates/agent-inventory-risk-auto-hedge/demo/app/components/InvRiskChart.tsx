"use client";

import type { InvRiskState } from "@/lib/invrisk-engine";

type Props = Readonly<{ agent: InvRiskState | null }>;

const W = 720, H = 340;

export function InvRiskChart({ agent }: Props) {
  if (!agent) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start monitoring to see the balance band.</div>;
  }

  const cfg = agent.config;
  const hist = agent.balanceHistory;
  if (hist.length === 0) return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>Warming up…</div>;

  const pad = { left: 60, right: 20, top: 20, bottom: 40 };
  const chartW = W - pad.left - pad.right;
  const chartH = H - pad.top - pad.bottom;

  // Y-axis range — always include hard band edges + at least 20% headroom around observed extremes.
  const observed = hist.map((h) => h.balance);
  const minObs = Math.min(...observed, cfg.target - cfg.hardTolerance * 1.2);
  const maxObs = Math.max(...observed, cfg.target + cfg.hardTolerance * 1.2);
  const range = Math.max(maxObs - minObs, cfg.hardTolerance * 2);
  const yMin = minObs - range * 0.05;
  const yMax = maxObs + range * 0.05;

  const yFor = (v: number) => pad.top + (1 - (v - yMin) / (yMax - yMin)) * chartH;
  const xFor = (i: number) => pad.left + (i / Math.max(1, hist.length - 1)) * chartW;

  const targetY = yFor(cfg.target);
  const softTopY = yFor(cfg.target + cfg.softTolerance);
  const softBotY = yFor(cfg.target - cfg.softTolerance);
  const hardTopY = yFor(cfg.target + cfg.hardTolerance);
  const hardBotY = yFor(cfg.target - cfg.hardTolerance);

  const balancePath = hist.map((h, i) => `${i === 0 ? "M" : "L"} ${xFor(i)} ${yFor(h.balance)}`).join(" ");

  // Signal dots
  const startT = hist[0].t;
  const endT = hist[hist.length - 1].t;
  const tSpan = Math.max(1, endT - startT);
  const dots = agent.recentSignals
    .filter((s) => s.t >= startT && s.t <= endT)
    .map((s) => {
      const x = pad.left + ((s.t - startT) / tSpan) * chartW;
      const y = yFor(s.balance);
      const color = s.hedgeFired ? "var(--neg)" : s.zone === "soft_band" ? "var(--accent)" : s.zone === "hard_band" ? "#ff9f43" : "var(--pos)";
      return { x, y, color, hedge: s.hedgeFired };
    });

  return (
    <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 360 }}>
      <rect x={0} y={0} width={W} height={H} fill="transparent" />

      {/* Hard band (danger, red-shaded) */}
      <rect x={pad.left} y={pad.top} width={chartW} height={hardTopY - pad.top} fill="rgba(239,68,68,0.10)" />
      <rect x={pad.left} y={hardBotY} width={chartW} height={pad.top + chartH - hardBotY} fill="rgba(239,68,68,0.10)" />

      {/* Soft band (warn, orange-shaded, thin) */}
      <rect x={pad.left} y={hardTopY} width={chartW} height={softTopY - hardTopY} fill="rgba(255,138,28,0.10)" />
      <rect x={pad.left} y={softBotY} width={chartW} height={hardBotY - softBotY} fill="rgba(255,138,28,0.10)" />

      {/* OK band (green, thin) */}
      <rect x={pad.left} y={softTopY} width={chartW} height={softBotY - softTopY} fill="rgba(34,197,94,0.06)" />

      {/* Threshold lines */}
      <line x1={pad.left} x2={pad.left + chartW} y1={hardTopY} y2={hardTopY} stroke="var(--neg)" strokeWidth={0.8} strokeDasharray="3 3" />
      <line x1={pad.left} x2={pad.left + chartW} y1={hardBotY} y2={hardBotY} stroke="var(--neg)" strokeWidth={0.8} strokeDasharray="3 3" />
      <line x1={pad.left} x2={pad.left + chartW} y1={softTopY} y2={softTopY} stroke="var(--accent)" strokeWidth={0.8} strokeDasharray="3 3" />
      <line x1={pad.left} x2={pad.left + chartW} y1={softBotY} y2={softBotY} stroke="var(--accent)" strokeWidth={0.8} strokeDasharray="3 3" />
      <line x1={pad.left} x2={pad.left + chartW} y1={targetY} y2={targetY} stroke="var(--pos)" strokeWidth={1} strokeDasharray="6 4" />

      {/* Balance line */}
      <path d={balancePath} fill="none" stroke="var(--accent)" strokeWidth={1.4} />

      {/* Signal dots */}
      {dots.map((d, i) => (
        <circle key={i} cx={d.x} cy={d.y} r={d.hedge ? 4 : 2.5} fill={d.color} stroke={d.hedge ? "var(--bg)" : "none"} strokeWidth={d.hedge ? 1 : 0} />
      ))}

      {/* Y-axis labels */}
      <text x={pad.left - 6} y={targetY + 4} fill="var(--pos)" fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="end">target {cfg.target}</text>
      <text x={pad.left - 6} y={hardTopY + 4} fill="var(--neg)" fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="end">+{cfg.hardTolerance}</text>
      <text x={pad.left - 6} y={hardBotY + 4} fill="var(--neg)" fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="end">-{cfg.hardTolerance}</text>
      <text x={pad.left - 6} y={softTopY + 4} fill="var(--accent)" fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="end">+{cfg.softTolerance}</text>
      <text x={pad.left - 6} y={softBotY + 4} fill="var(--accent)" fontSize={10} fontFamily="ui-monospace, monospace" textAnchor="end">-{cfg.softTolerance}</text>

      {/* Legend */}
      <g transform={`translate(${pad.left}, ${H - 8})`}>
        <rect x={0} y={-8} width={10} height={5} fill="rgba(34,197,94,0.4)" />
        <text x={16} y={-3} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">OK</text>
        <rect x={60} y={-8} width={10} height={5} fill="rgba(255,138,28,0.4)" />
        <text x={76} y={-3} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">soft band</text>
        <rect x={160} y={-8} width={10} height={5} fill="rgba(239,68,68,0.4)" />
        <text x={176} y={-3} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">hard band</text>
        <circle cx={264} cy={-5} r={3.5} fill="var(--neg)" stroke="var(--bg)" strokeWidth={1} />
        <text x={274} y={-3} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">hedge fired</text>
      </g>
    </svg>
  );
}
