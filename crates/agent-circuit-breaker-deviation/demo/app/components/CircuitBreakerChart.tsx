"use client";

import type { Tick } from "@/lib/store";
import type { CircuitBreakerState } from "@/lib/circuitbreaker-engine";

type Props = Readonly<{ ticks: readonly Tick[]; circuitbreaker: CircuitBreakerState | null }>;

const W = 720, H = 300, PAD_L = 60, PAD_R = 20, PAD_T = 14, PAD_B = 26;

export function CircuitBreakerChart({ ticks, circuitbreaker }: Props) {
  if (ticks.length === 0 || !circuitbreaker) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start Circuit Breaker to see mid, baseline, bands and breaches.</div>;
  }
  const dev = circuitbreaker.config.maxDeviationPct / 100;
  const prices = ticks.map((t) => t.price);
  const baselines = ticks.map((t) => t.baseline).filter((v): v is number => v !== null);
  const upper = baselines.map((v) => v * (1 + dev));
  const lower = baselines.map((v) => v * (1 - dev));

  const minAll = Math.min(...prices, ...(baselines.length ? baselines : prices), ...(lower.length ? lower : prices));
  const maxAll = Math.max(...prices, ...(baselines.length ? baselines : prices), ...(upper.length ? upper : prices));
  const pad = (maxAll - minAll) * 0.1 || minAll * 0.02;
  const yMin = minAll - pad, yMax = maxAll + pad, yRange = yMax - yMin || 1;
  const tMin = ticks[0].t, tMax = ticks[ticks.length - 1].t;
  const tRange = tMax - tMin || 1;
  const x = (t: number) => PAD_L + ((t - tMin) / tRange) * (W - PAD_L - PAD_R);
  const y = (v: number) => PAD_T + (1 - (v - yMin) / yRange) * (H - PAD_T - PAD_B);

  const pricePath = ticks.map((t, i) => `${i === 0 ? "M" : "L"}${x(t.t).toFixed(1)},${y(t.price).toFixed(1)}`).join(" ");
  const baselinePath = ticks
    .map((t, i) => t.baseline === null ? null : `${i === 0 ? "M" : "L"}${x(t.t).toFixed(1)},${y(t.baseline).toFixed(1)}`)
    .filter((s): s is string => s !== null)
    .join(" ");
  const upperPath = ticks
    .map((t, i) => t.baseline === null ? null : `${i === 0 ? "M" : "L"}${x(t.t).toFixed(1)},${y(t.baseline * (1 + dev)).toFixed(1)}`)
    .filter((s): s is string => s !== null)
    .join(" ");
  const lowerPath = ticks
    .map((t, i) => t.baseline === null ? null : `${i === 0 ? "M" : "L"}${x(t.t).toFixed(1)},${y(t.baseline * (1 - dev)).toFixed(1)}`)
    .filter((s): s is string => s !== null)
    .join(" ");

  const digits = yMax > 100 ? 2 : yMax > 1 ? 4 : 6;
  const yTicks: number[] = [];
  for (let i = 0; i <= 4; i++) yTicks.push(yMin + (yRange * i) / 4);

  const lastPrice = ticks[ticks.length - 1].price;
  const chartXMin = PAD_L;
  const chartXMax = W - PAD_R;
  const clampX = (px: number) => Math.max(chartXMin, Math.min(chartXMax, px));

  // Compute paused shaded intervals visible in current window.
  const pauseRects = (circuitbreaker.pauseIntervals ?? [])
    .map((p) => {
      const startPx = clampX(x(p.start));
      const endPx = clampX(x(Math.min(p.end, tMax)));
      const w = Math.max(0, endPx - startPx);
      return { startPx, w };
    })
    .filter((r) => r.w > 0);

  return (
    <svg viewBox={`0 0 ${W + 90} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 340 }}>
      <rect x={0} y={0} width={W + 90} height={H} fill="transparent" />

      {/* Paused shaded intervals (behind everything) */}
      {pauseRects.map((r, i) => (
        <rect key={`ps-${i}`} x={r.startPx} y={PAD_T} width={r.w} height={H - PAD_T - PAD_B} fill="#7a2929" opacity={0.15} />
      ))}

      {yTicks.map((v, i) => (
        <g key={i}>
          <line x1={PAD_L} x2={W - PAD_R} y1={y(v)} y2={y(v)} stroke="#22222c" strokeWidth={1} />
          <text x={PAD_L - 8} y={y(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{v.toFixed(digits)}</text>
        </g>
      ))}

      {/* Deviation bands (upper/lower) — red dashed */}
      {upperPath && <path d={upperPath} stroke="#e05a5a" strokeWidth={1.2} strokeDasharray="4,3" fill="none" opacity={0.85} />}
      {lowerPath && <path d={lowerPath} stroke="#e05a5a" strokeWidth={1.2} strokeDasharray="4,3" fill="none" opacity={0.85} />}

      {/* Baseline (orange) */}
      {baselinePath && <path d={baselinePath} stroke="#f0a63a" strokeWidth={1.6} fill="none" opacity={0.95} />}

      {/* Mid price (white) */}
      <path d={pricePath} stroke="#ececf1" strokeWidth={1.6} fill="none" />

      {/* Breach markers: vertical red X */}
      {(circuitbreaker.breachEvents ?? []).map((b) => {
        const bx = clampX(x(b.t));
        return (
          <g key={`br-${b.seq}`}>
            <line x1={bx} x2={bx} y1={PAD_T} y2={H - PAD_B} stroke="#ff4d4d" strokeWidth={1} strokeDasharray="2,3" opacity={0.85} />
            <line x1={bx - 5} x2={bx + 5} y1={y(b.price) - 5} y2={y(b.price) + 5} stroke="#ff4d4d" strokeWidth={1.6} />
            <line x1={bx - 5} x2={bx + 5} y1={y(b.price) + 5} y2={y(b.price) - 5} stroke="#ff4d4d" strokeWidth={1.6} />
          </g>
        );
      })}

      <circle cx={x(tMax)} cy={y(lastPrice)} r={3.5} fill="#ececf1" />

      {/* Right-margin labels */}
      {circuitbreaker.baseline !== null && (
        <text x={W - PAD_R + 4} y={y(circuitbreaker.baseline) + 3} fill="#f0a63a" fontSize={10} fontFamily="ui-monospace, monospace">base {circuitbreaker.baseline.toFixed(digits)}</text>
      )}
      {circuitbreaker.baseline !== null && (
        <>
          <text x={W - PAD_R + 4} y={y(circuitbreaker.baseline * (1 + dev)) + 3} fill="#e05a5a" fontSize={10} fontFamily="ui-monospace, monospace">+{circuitbreaker.config.maxDeviationPct}%</text>
          <text x={W - PAD_R + 4} y={y(circuitbreaker.baseline * (1 - dev)) + 3} fill="#e05a5a" fontSize={10} fontFamily="ui-monospace, monospace">−{circuitbreaker.config.maxDeviationPct}%</text>
        </>
      )}
    </svg>
  );
}
