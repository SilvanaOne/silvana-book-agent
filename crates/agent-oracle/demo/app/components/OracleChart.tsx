"use client";

import type { MarketTick } from "@/lib/store";
import type { OracleState } from "@/lib/oracle-engine";

type Props = Readonly<{ ticks: readonly MarketTick[]; oracle: OracleState | null }>;

const W = 720, H = 300, PAD_L = 60, PAD_R = 90, PAD_T = 14, PAD_B = 26;

// Distinct line colors per market (cycled if more than 6).
const LINE_COLORS = ["#7dd3fc", "#fca5a5", "#a5f3d0", "#fcd34d", "#c4b5fd", "#f0abfc"];

export function OracleChart({ ticks, oracle }: Props) {
  if (ticks.length === 0 || !oracle) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start Oracle to see published price feeds.</div>;
  }

  // Group ticks by market, keep original order from config.
  const markets = oracle.config.markets;
  const byMarket = new Map<string, MarketTick[]>();
  for (const m of markets) byMarket.set(m, []);
  for (const t of ticks) {
    const arr = byMarket.get(t.market);
    if (arr) arr.push(t);
  }

  // Compute per-market baseline (first observed price) and normalized values.
  type Row = { market: string; color: string; normalized: { t: number; v: number; published: boolean }[]; last: number; baseline: number };
  const rows: Row[] = [];
  for (let i = 0; i < markets.length; i++) {
    const m = markets[i];
    const arr = byMarket.get(m) ?? [];
    if (arr.length === 0) continue;
    const baseline = arr[0].price;
    const normalized = arr.map((t) => ({ t: t.t, v: t.price / baseline, published: t.published }));
    rows.push({ market: m, color: LINE_COLORS[i % LINE_COLORS.length], normalized, last: arr[arr.length - 1].price, baseline });
  }

  if (rows.length === 0) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>Waiting for ticks…</div>;
  }

  // Y range across normalized values.
  let minAll = Infinity, maxAll = -Infinity;
  for (const r of rows) for (const p of r.normalized) { if (p.v < minAll) minAll = p.v; if (p.v > maxAll) maxAll = p.v; }
  const pad = (maxAll - minAll) * 0.1 || 0.02;
  const yMin = minAll - pad, yMax = maxAll + pad, yRange = yMax - yMin || 1;

  // X range: overall time span.
  let tMin = Infinity, tMax = -Infinity;
  for (const r of rows) {
    if (r.normalized[0].t < tMin) tMin = r.normalized[0].t;
    if (r.normalized[r.normalized.length - 1].t > tMax) tMax = r.normalized[r.normalized.length - 1].t;
  }
  const tRange = tMax - tMin || 1;
  const x = (t: number) => PAD_L + ((t - tMin) / tRange) * (W - PAD_L - PAD_R);
  const y = (v: number) => PAD_T + (1 - (v - yMin) / yRange) * (H - PAD_T - PAD_B);

  const yTicks: number[] = [];
  for (let i = 0; i <= 4; i++) yTicks.push(yMin + (yRange * i) / 4);

  return (
    <svg viewBox={`0 0 ${W + 90} ${H + 40}`} style={{ width: "100%", height: "auto", maxHeight: 360 }}>
      <rect x={0} y={0} width={W + 90} height={H + 40} fill="transparent" />
      {yTicks.map((v, i) => (
        <g key={i}>
          <line x1={PAD_L} x2={W - PAD_R} y1={y(v)} y2={y(v)} stroke="#22222c" strokeWidth={1} />
          <text x={PAD_L - 8} y={y(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{v.toFixed(3)}×</text>
        </g>
      ))}

      {rows.map((r) => {
        const path = r.normalized.map((p, i) => `${i === 0 ? "M" : "L"}${x(p.t).toFixed(1)},${y(p.v).toFixed(1)}`).join(" ");
        return (
          <g key={r.market}>
            <path d={path} stroke={r.color} strokeWidth={1.6} fill="none" opacity={0.95} />
            {/* Publish-event overlays: orange dots at points where published=true */}
            {r.normalized.filter((p) => p.published).map((p, i) => (
              <circle key={i} cx={x(p.t)} cy={y(p.v)} r={3} fill="#f59e0b" stroke="#f59e0b" strokeWidth={1} />
            ))}
            {/* Last-value marker */}
            <circle cx={x(r.normalized[r.normalized.length - 1].t)} cy={y(r.normalized[r.normalized.length - 1].v)} r={3} fill={r.color} />
            <text x={W - PAD_R + 4} y={y(r.normalized[r.normalized.length - 1].v) + 3} fill={r.color} fontSize={10} fontFamily="ui-monospace, monospace">{r.market} {r.last.toFixed(r.last > 100 ? 2 : r.last > 1 ? 4 : 6)}</text>
          </g>
        );
      })}

      {/* Legend for publish dots */}
      <g transform={`translate(${PAD_L}, ${H + 18})`}>
        <circle cx={0} cy={0} r={3} fill="#f59e0b" />
        <text x={8} y={4} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">publish event</text>
      </g>
    </svg>
  );
}
