"use client";

import type { Tick } from "@/lib/store";
import type { CashBufferState } from "@/lib/cashbuffer-engine";

type Props = Readonly<{ ticks: readonly Tick[]; cashbuffer: CashBufferState | null }>;

const W = 720, H = 300, PAD_L = 60, PAD_R = 20, PAD_T = 14, PAD_B = 26;

// Reconstruct the balance series from transfers + inflow between checks.
// The store only persists price ticks. We reconstruct the walk by working
// forward from starting balance: each tick adds incomeRate; each transfer
// snaps balance down to transfer.toBalance at its timestamp.
function reconstructBalances(ticks: readonly Tick[], cb: CashBufferState): number[] {
  const inflow = cb.config.incomeRate;
  const transfers = cb.transfers;
  let balance = cb.config.startingBalance;
  const out: number[] = [];
  let ti = 0;
  for (let i = 0; i < ticks.length; i++) {
    const t = ticks[i].t;
    // On the very first tick we've already seeded balance = startingBalance.
    if (i > 0) balance = balance + inflow;
    while (ti < transfers.length && transfers[ti].t <= t) {
      balance = transfers[ti].toBalance;
      ti += 1;
    }
    out.push(balance);
  }
  return out;
}

export function CashBufferChart({ ticks, cashbuffer }: Props) {
  if (ticks.length === 0 || !cashbuffer) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start Cash Buffer to see balance, band and pushes.</div>;
  }

  const balances = reconstructBalances(ticks, cashbuffer);
  const { minCc, maxCc } = cashbuffer.config;
  const target = cashbuffer.target;

  const minAll = Math.min(...balances, minCc);
  const maxAll = Math.max(...balances, maxCc);
  const pad = (maxAll - minAll) * 0.15 || Math.max(1, minAll * 0.05);
  const yMin = minAll - pad, yMax = maxAll + pad, yRange = yMax - yMin || 1;
  const tMin = ticks[0].t, tMax = ticks[ticks.length - 1].t;
  const tRange = tMax - tMin || 1;
  const x = (t: number) => PAD_L + ((t - tMin) / tRange) * (W - PAD_L - PAD_R);
  const y = (v: number) => PAD_T + (1 - (v - yMin) / yRange) * (H - PAD_T - PAD_B);

  const balancePath = ticks.map((t, i) => `${i === 0 ? "M" : "L"}${x(t.t).toFixed(1)},${y(balances[i]).toFixed(1)}`).join(" ");

  const digits = yMax > 100 ? 2 : yMax > 1 ? 4 : 6;
  const yTicks: number[] = [];
  for (let i = 0; i <= 4; i++) yTicks.push(yMin + (yRange * i) / 4);

  const lastBalance = balances[balances.length - 1];

  // Push marker: downward triangle at push timestamp on the balance line.
  const triangle = (cx: number, cy: number, r: number) =>
    `M${cx.toFixed(1)},${(cy + r).toFixed(1)} L${(cx - r).toFixed(1)},${(cy - r).toFixed(1)} L${(cx + r).toFixed(1)},${(cy - r).toFixed(1)} Z`;

  return (
    <svg viewBox={`0 0 ${W + 90} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 340 }}>
      <rect x={0} y={0} width={W + 90} height={H} fill="transparent" />
      {yTicks.map((v, i) => (
        <g key={i}>
          <line x1={PAD_L} x2={W - PAD_R} y1={y(v)} y2={y(v)} stroke="#22222c" strokeWidth={1} />
          <text x={PAD_L - 8} y={y(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{v.toFixed(digits)}</text>
        </g>
      ))}

      {/* Band lines: min (green), max (red), target (orange dashed) */}
      <line x1={PAD_L} x2={W - PAD_R} y1={y(maxCc)} y2={y(maxCc)} stroke="var(--neg)" strokeWidth={1.4} strokeDasharray="5,3" />
      <line x1={PAD_L} x2={W - PAD_R} y1={y(minCc)} y2={y(minCc)} stroke="var(--pos)" strokeWidth={1.4} strokeDasharray="5,3" />
      <line x1={PAD_L} x2={W - PAD_R} y1={y(target)} y2={y(target)} stroke="var(--accent)" strokeWidth={1.2} strokeDasharray="2,4" opacity={0.9} />

      {/* Balance line */}
      <path d={balancePath} stroke="#ececf1" strokeWidth={1.8} fill="none" />

      {/* Push markers: orange triangles at (t, toBalance) */}
      {cashbuffer.transfers.map((tr) => {
        if (tr.t < tMin || tr.t > tMax) return null;
        return (
          <g key={`push-${tr.seq}`}>
            <path d={triangle(x(tr.t), y(tr.toBalance) - 8, 5)} fill="var(--accent)" stroke="var(--accent)" strokeWidth={1} />
            <line x1={x(tr.t)} x2={x(tr.t)} y1={y(tr.fromBalance)} y2={y(tr.toBalance)} stroke="var(--accent)" strokeWidth={1} strokeDasharray="2,2" opacity={0.7} />
          </g>
        );
      })}

      {/* Warning markers: yellow dots on the min line */}
      {cashbuffer.warnings.map((w) => {
        if (w.t < tMin || w.t > tMax) return null;
        return <circle key={`warn-${w.seq}`} cx={x(w.t)} cy={y(minCc)} r={4} fill="#f5c518" stroke="#f5c518" />;
      })}

      {/* Current balance dot */}
      <circle cx={x(tMax)} cy={y(lastBalance)} r={3.5} fill="#ececf1" />

      {/* Right-margin labels */}
      <text x={W - PAD_R + 4} y={y(maxCc) + 3} fill="var(--neg)" fontSize={10} fontFamily="ui-monospace, monospace">max {maxCc.toFixed(digits)}</text>
      <text x={W - PAD_R + 4} y={y(target) + 3} fill="var(--accent)" fontSize={10} fontFamily="ui-monospace, monospace">target {target.toFixed(digits)}</text>
      <text x={W - PAD_R + 4} y={y(minCc) + 3} fill="var(--pos)" fontSize={10} fontFamily="ui-monospace, monospace">min {minCc.toFixed(digits)}</text>
    </svg>
  );
}
