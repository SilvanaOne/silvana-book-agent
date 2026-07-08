"use client";

import type { Tick } from "@/lib/store";
import type { FairValueState } from "@/lib/fairvalue-engine";

type Props = Readonly<{ ticks: readonly Tick[]; fairvalue: FairValueState | null }>;

const W = 720, H = 300, PAD_L = 60, PAD_R = 90, PAD_T = 14, PAD_B = 26;

// A few distinct colors for source lines. Rotates if more sources than colors.
const SOURCE_COLORS = ["#7aa2ff", "#f6c177", "#c4a7e7", "#9ccfd8", "#eb6f92", "#a3be8c"];

export function FairValueChart({ ticks, fairvalue }: Props) {
  if (ticks.length === 0 || !fairvalue) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start Fair Value to see source feeds, true mid and aggregated fair value.</div>;
  }

  const sources = fairvalue.config.sources;
  const truthSeries = ticks.map((t) => t.price);
  const fairSeries = ticks.map((t) => t.fair).filter((v): v is number => v !== null);

  // Per-source series
  const perSource: Record<string, number[]> = {};
  for (const s of sources) perSource[s] = [];
  for (const t of ticks) {
    for (const s of sources) {
      const q = t.sources.find((x) => x.name === s);
      if (q) perSource[s].push(q.price);
    }
  }

  const all: number[] = [...truthSeries, ...fairSeries];
  for (const s of sources) all.push(...perSource[s]);

  const minAll = Math.min(...all);
  const maxAll = Math.max(...all);
  const pad = (maxAll - minAll) * 0.1 || minAll * 0.02;
  const yMin = minAll - pad, yMax = maxAll + pad, yRange = yMax - yMin || 1;
  const tMin = ticks[0].t, tMax = ticks[ticks.length - 1].t;
  const tRange = tMax - tMin || 1;
  const x = (t: number) => PAD_L + ((t - tMin) / tRange) * (W - PAD_L - PAD_R);
  const y = (v: number) => PAD_T + (1 - (v - yMin) / yRange) * (H - PAD_T - PAD_B);

  const truthPath = ticks.map((t, i) => `${i === 0 ? "M" : "L"}${x(t.t).toFixed(1)},${y(t.price).toFixed(1)}`).join(" ");

  // Fair line only where fair !== null.
  const fairPathSegs: string[] = [];
  let started = false;
  for (const t of ticks) {
    if (t.fair === null) { started = false; continue; }
    fairPathSegs.push(`${!started ? "M" : "L"}${x(t.t).toFixed(1)},${y(t.fair).toFixed(1)}`);
    started = true;
  }
  const fairPath = fairPathSegs.join(" ");

  const sourcePaths = sources.map((name) => {
    const segs = ticks
      .map((t) => {
        const q = t.sources.find((x) => x.name === name);
        if (!q) return null;
        return { t: t.t, p: q.price };
      })
      .filter((v): v is { t: number; p: number } => v !== null);
    return {
      name,
      d: segs.map((s, i) => `${i === 0 ? "M" : "L"}${x(s.t).toFixed(1)},${y(s.p).toFixed(1)}`).join(" "),
    };
  });

  const digits = yMax > 100 ? 2 : yMax > 1 ? 4 : 6;
  const yTicks: number[] = [];
  for (let i = 0; i <= 4; i++) yTicks.push(yMin + (yRange * i) / 4);

  const lastTick = ticks[ticks.length - 1];

  return (
    <svg viewBox={`0 0 ${W + 90} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 340 }}>
      <rect x={0} y={0} width={W + 90} height={H} fill="transparent" />
      {yTicks.map((v, i) => (
        <g key={i}>
          <line x1={PAD_L} x2={W - PAD_R} y1={y(v)} y2={y(v)} stroke="#22222c" strokeWidth={1} />
          <text x={PAD_L - 8} y={y(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{v.toFixed(digits)}</text>
        </g>
      ))}

      {/* Source lines — thin, low opacity */}
      {sourcePaths.map((sp, i) => (
        <path key={sp.name} d={sp.d} stroke={SOURCE_COLORS[i % SOURCE_COLORS.length]} strokeWidth={1} fill="none" opacity={0.55} />
      ))}

      {/* True mid — thin white line */}
      <path d={truthPath} stroke="#ececf1" strokeWidth={1.2} fill="none" opacity={0.85} strokeDasharray="4,3" />

      {/* Fair value — thick orange (accent) line */}
      {fairPath && <path d={fairPath} stroke="var(--accent)" strokeWidth={2.4} fill="none" />}

      {/* Live dot on aggregated fair value */}
      {fairvalue.fairValue !== null && (
        <circle cx={x(tMax)} cy={y(fairvalue.fairValue)} r={4} fill="var(--accent)" />
      )}

      {/* Legend — right margin */}
      <g fontSize={10} fontFamily="ui-monospace, monospace">
        <text x={W - PAD_R + 8} y={PAD_T + 10} fill="var(--accent)">■ fair {fairvalue.fairValue !== null ? fairvalue.fairValue.toFixed(digits) : "—"}</text>
        <text x={W - PAD_R + 8} y={PAD_T + 24} fill="#ececf1">- - true {lastTick.price.toFixed(digits)}</text>
        {sources.map((name, i) => {
          const q = lastTick.sources.find((x) => x.name === name);
          return (
            <text key={name} x={W - PAD_R + 8} y={PAD_T + 40 + i * 14} fill={SOURCE_COLORS[i % SOURCE_COLORS.length]}>
              — {name.length > 10 ? name.slice(0, 10) : name} {q ? q.price.toFixed(digits) : "—"}
            </text>
          );
        })}
      </g>
    </svg>
  );
}
