"use client";

import type { PortfolioRebalancingState, WeightView } from "@/lib/portfoliorebalancing-engine";

type Props = Readonly<{ portfoliorebalancing: PortfolioRebalancingState | null }>;

const PIE_R = 90;
const PIE_CX = 110;
const PIE_CY = 110;
const CHART_H = 240;

const PALETTE = [
  "#5b8def",
  "#e5a54b",
  "#8b5cf6",
  "#22c55e",
  "#ef4444",
  "#14b8a6",
  "#f59e0b",
  "#ec4899",
];

function colorFor(i: number): string {
  return PALETTE[i % PALETTE.length];
}

type PieSlice = { start: number; end: number; color: string; label: string; value: number };

function pieSlices(values: number[], labels: string[]): PieSlice[] {
  const total = values.reduce((s, v) => s + Math.max(0, v), 0);
  if (total <= 0) return [];
  const slices: PieSlice[] = [];
  let acc = 0;
  for (let i = 0; i < values.length; i++) {
    const v = Math.max(0, values[i]);
    const frac = v / total;
    const start = acc;
    const end = acc + frac;
    slices.push({ start, end, color: colorFor(i), label: labels[i], value: v });
    acc = end;
  }
  return slices;
}

function arcPath(start: number, end: number): string {
  if (end - start <= 0) return "";
  if (end - start >= 0.9999) {
    // Full circle — draw as two arcs.
    return `M ${PIE_CX + PIE_R},${PIE_CY} A ${PIE_R},${PIE_R} 0 1 1 ${PIE_CX - PIE_R},${PIE_CY} A ${PIE_R},${PIE_R} 0 1 1 ${PIE_CX + PIE_R},${PIE_CY} Z`;
  }
  const a1 = start * 2 * Math.PI - Math.PI / 2;
  const a2 = end * 2 * Math.PI - Math.PI / 2;
  const x1 = PIE_CX + PIE_R * Math.cos(a1);
  const y1 = PIE_CY + PIE_R * Math.sin(a1);
  const x2 = PIE_CX + PIE_R * Math.cos(a2);
  const y2 = PIE_CY + PIE_R * Math.sin(a2);
  const large = end - start > 0.5 ? 1 : 0;
  return `M ${PIE_CX},${PIE_CY} L ${x1.toFixed(2)},${y1.toFixed(2)} A ${PIE_R},${PIE_R} 0 ${large} 1 ${x2.toFixed(2)},${y2.toFixed(2)} Z`;
}

function Pie({ title, values, labels }: { title: string; values: number[]; labels: string[] }) {
  const slices = pieSlices(values, labels);
  return (
    <div style={{ display: "flex", flexDirection: "column", alignItems: "center", gap: 6 }}>
      <div className="mono" style={{ fontSize: 12, color: "var(--text-faint)" }}>{title}</div>
      <svg viewBox={`0 0 220 220`} style={{ width: "100%", maxWidth: 220, height: "auto" }}>
        <circle cx={PIE_CX} cy={PIE_CY} r={PIE_R} fill="var(--bg-card)" stroke="#22222c" strokeWidth={1} />
        {slices.map((s, i) => (
          <path key={i} d={arcPath(s.start, s.end)} fill={s.color} opacity={0.85} stroke="#111" strokeWidth={0.6} />
        ))}
        <circle cx={PIE_CX} cy={PIE_CY} r={PIE_R * 0.5} fill="var(--bg)" />
      </svg>
      <div style={{ display: "flex", flexWrap: "wrap", justifyContent: "center", gap: "4px 12px" }}>
        {slices.map((s, i) => (
          <div key={i} className="mono" style={{ fontSize: 11, display: "flex", alignItems: "center", gap: 4 }}>
            <span style={{ width: 8, height: 8, background: s.color, display: "inline-block", borderRadius: 2 }} />
            {s.label} {(s.value * 100).toFixed(1)}%
          </div>
        ))}
      </div>
    </div>
  );
}

function DeviationBars({ weights }: { weights: readonly WeightView[] }) {
  if (weights.length === 0) return null;
  const maxAbs = Math.max(1, ...weights.map((w) => Math.abs(w.deviationPct)));
  const W = 720;
  const rowH = 26;
  const H = weights.length * rowH + 12;
  const labelW = 90;
  const midX = labelW + 20;
  const barW = W - midX - 60;
  const halfW = barW / 2;
  const center = midX + halfW;

  return (
    <div style={{ marginTop: 14 }}>
      <div className="mono" style={{ fontSize: 12, color: "var(--text-faint)", marginBottom: 6, textAlign: "center" }}>
        Weight deviation vs target (percentage points) — green = over, red = under
      </div>
      <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 260 }}>
        <line x1={center} x2={center} y1={4} y2={H - 4} stroke="#22222c" strokeWidth={1} />
        {weights.map((w, i) => {
          const y = 8 + i * rowH;
          const dev = w.deviationPct;
          const w2 = (Math.abs(dev) / maxAbs) * halfW;
          const isOver = dev > 0;
          const color = isOver ? "var(--pos, #22c55e)" : "var(--neg, #ef4444)";
          const bx = isOver ? center : center - w2;
          return (
            <g key={w.instrument}>
              <text x={labelW} y={y + rowH / 2 + 3} textAnchor="end" fontSize={11} fontFamily="ui-monospace, monospace" fill="var(--text)">{w.instrument}</text>
              <rect x={bx} y={y + 4} width={w2} height={rowH - 10} fill={color} opacity={0.85} />
              <text
                x={isOver ? bx + w2 + 4 : bx - 4}
                y={y + rowH / 2 + 3}
                textAnchor={isOver ? "start" : "end"}
                fontSize={11}
                fontFamily="ui-monospace, monospace"
                fill="var(--text-faint)"
              >
                {(dev >= 0 ? "+" : "") + dev.toFixed(2)}pp
              </text>
            </g>
          );
        })}
      </svg>
    </div>
  );
}

export function PortfolioRebalancingChart({ portfoliorebalancing }: Props) {
  if (!portfoliorebalancing || portfoliorebalancing.weights.length === 0) {
    return (
      <div style={{ height: CHART_H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>
        No data — start Rebalancing to compare current vs target weights.
      </div>
    );
  }
  const labels = portfoliorebalancing.weights.map((w) => w.instrument);
  const current = portfoliorebalancing.weights.map((w) => w.currentWeight);
  const target = portfoliorebalancing.weights.map((w) => w.targetWeight);

  return (
    <div>
      <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 16 }}>
        <Pie title="Current weights" values={current} labels={labels} />
        <Pie title="Target weights" values={target} labels={labels} />
      </div>
      <DeviationBars weights={portfoliorebalancing.weights} />
    </div>
  );
}
