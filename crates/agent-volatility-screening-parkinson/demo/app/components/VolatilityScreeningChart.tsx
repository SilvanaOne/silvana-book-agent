"use client";

import type { Tick } from "@/lib/store";
import type { VolatilityScreeningState } from "@/lib/volatilityscreening-engine";

type Props = Readonly<{ ticks: readonly Tick[]; volatilityscreening: VolatilityScreeningState | null }>;

const W = 720, H = 320, PAD_L = 60, PAD_R = 64, PAD_T = 14, PAD_B = 26;

export function VolatilityScreeningChart({ ticks, volatilityscreening }: Props) {
  if (ticks.length === 0 || !volatilityscreening) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start Volatility to see mid + Parkinson sigma overlay.</div>;
  }

  const prices = ticks.map((t) => t.price);
  const vols = ticks
    .map((t) => t.sigmaAnnualized)
    .filter((v): v is number => v !== null && Number.isFinite(v));

  const pMin = Math.min(...prices), pMax = Math.max(...prices);
  const pPad = (pMax - pMin) * 0.1 || pMin * 0.02;
  const pLo = pMin - pPad, pHi = pMax + pPad, pRange = pHi - pLo || 1;

  // Right axis: annualized sigma in percent (0..vHi).
  const vHiRaw = vols.length > 0 ? Math.max(...vols) : 0;
  const vHi = Math.max(0.1, vHiRaw * 1.2);
  const vLo = 0;
  const vRange = vHi - vLo || 1;

  const tMin = ticks[0].t, tMax = ticks[ticks.length - 1].t;
  const tRange = tMax - tMin || 1;

  const x = (t: number) => PAD_L + ((t - tMin) / tRange) * (W - PAD_L - PAD_R);
  const yP = (v: number) => PAD_T + (1 - (v - pLo) / pRange) * (H - PAD_T - PAD_B);
  const yV = (v: number) => PAD_T + (1 - (v - vLo) / vRange) * (H - PAD_T - PAD_B);

  const pricePath = ticks.map((t, i) => `${i === 0 ? "M" : "L"}${x(t.t).toFixed(1)},${yP(t.price).toFixed(1)}`).join(" ");
  const volPath = ticks
    .map((t, i) => t.sigmaAnnualized === null ? null : `${i === 0 ? "M" : "L"}${x(t.t).toFixed(1)},${yV(t.sigmaAnnualized).toFixed(1)}`)
    .filter((s): s is string => s !== null)
    .join(" ");

  // Bar-close markers along the bottom.
  const barCloseTicks = ticks.filter((t) => t.barClosed);

  const pDigits = pHi > 100 ? 2 : pHi > 1 ? 4 : 6;
  const yTicks: number[] = [];
  for (let i = 0; i <= 4; i++) yTicks.push(pLo + (pRange * i) / 4);
  const vTicks: number[] = [];
  for (let i = 0; i <= 4; i++) vTicks.push(vLo + (vRange * i) / 4);

  const lastPrice = ticks[ticks.length - 1].price;
  const lastVol = volatilityscreening.sigmaAnnualized;

  return (
    <svg viewBox={`0 0 ${W + 20} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 360 }}>
      <rect x={0} y={0} width={W + 20} height={H} fill="transparent" />

      {/* Left axis grid (price) */}
      {yTicks.map((v, i) => (
        <g key={`p${i}`}>
          <line x1={PAD_L} x2={W - PAD_R} y1={yP(v)} y2={yP(v)} stroke="#22222c" strokeWidth={1} />
          <text x={PAD_L - 8} y={yP(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{v.toFixed(pDigits)}</text>
        </g>
      ))}

      {/* Right axis (sigma %) */}
      {vTicks.map((v, i) => (
        <g key={`v${i}`}>
          <text x={W - PAD_R + 6} y={yV(v) + 3} fill="var(--accent)" fontSize={10} textAnchor="start" fontFamily="ui-monospace, monospace">{(v * 100).toFixed(1)}%</text>
        </g>
      ))}

      {/* Bar-close markers */}
      {barCloseTicks.map((t, i) => (
        <line
          key={`bc${i}`}
          x1={x(t.t)} x2={x(t.t)}
          y1={PAD_T} y2={H - PAD_B}
          stroke="#4a6cf7" strokeWidth={1} strokeDasharray="2 3" opacity={0.35}
        />
      ))}

      {/* Parkinson sigma overlay (orange) — right axis */}
      {volPath && <path d={volPath} stroke="var(--accent)" strokeWidth={1.8} fill="none" opacity={0.9} />}

      {/* Mid price (white) — left axis */}
      <path d={pricePath} stroke="#ececf1" strokeWidth={1.6} fill="none" />

      {/* Last point markers */}
      <circle cx={x(tMax)} cy={yP(lastPrice)} r={3.5} fill="#ececf1" />
      {lastVol !== null && Number.isFinite(lastVol) && (
        <circle cx={x(tMax)} cy={yV(lastVol)} r={3.5} fill="var(--accent)" />
      )}

      {/* Legend */}
      <g transform={`translate(${PAD_L + 4}, ${PAD_T + 4})`}>
        <line x1={0} y1={6} x2={18} y2={6} stroke="#ececf1" strokeWidth={1.6} />
        <text x={22} y={9} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">mid</text>
        <line x1={60} y1={6} x2={78} y2={6} stroke="var(--accent)" strokeWidth={1.8} />
        <text x={82} y={9} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">sigma ann. (Parkinson)</text>
      </g>
    </svg>
  );
}
